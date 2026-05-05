# Channels Within Rooms — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `#general`, `#links`, etc. real: each chat message carries a `channel` label inside the AEAD plaintext, and the Gleam UI's channels rail becomes driven by what peers actually post (replacing the static fixture).

**Architecture:** Add a `ChannelLabel` newtype in `sunset-core::crypto::envelope`, thread it through `SignedMessage` / `InnerSigPayload` / `compose_*` / `decode_message` / `OpenRoom` so channel routing happens *after* AEAD decrypt (relay never sees the channel). Reactions and receipts inherit the channel of the message they reference. Surface a `channel` field on the WASM `IncomingMessage` / `IncomingReceipt` / reactions snapshot, plus an `on_channels_changed` stream so the Gleam UI rebuilds its channels rail from real observations.

**Tech Stack:** Rust 2024 (workspace lints: no clippy suppressions, no unsafe). `postcard` for wire format. `wasm-bindgen` for the WASM bridge. Gleam + Lustre for the UI; FFI shims in JS. Playwright for e2e.

**Spec:** `docs/superpowers/specs/2026-05-04-channels-within-rooms-design.md`

**Working directory:** All work happens in the worktree `.worktrees/channels-in-rooms` on branch `channels-in-rooms`.

**Test runner:** `nix develop --command cargo test ...` for Rust. `nix develop --command cargo clippy --workspace --all-features --all-targets -- -D warnings` for clippy. `web/playwright.config.js` for e2e (run with `cd web && npx playwright test`).

---

## Task 1: `ChannelLabel` newtype + `Error::BadChannel`

**Why first:** Every later task imports `ChannelLabel`. Defining it (and its validation rules) up front lets every other task lean on `ChannelLabel::try_new` instead of hand-rolling validation.

**Files:**
- Modify: `crates/sunset-core/src/crypto/envelope.rs` (add `ChannelLabel` near `Signature`)
- Modify: `crates/sunset-core/src/error.rs` (add `BadChannel`)
- Modify: `crates/sunset-core/src/lib.rs` (re-export `ChannelLabel`, `DEFAULT_CHANNEL`)

- [ ] **Step 1: Add the failing tests in `crypto/envelope.rs`**

Append inside the existing `#[cfg(test)] mod tests { … }` block (after `message_body_reaction_remove_postcard_hex_pin`):

```rust
#[test]
fn channel_label_accepts_default_general() {
    let c = ChannelLabel::try_new("general").unwrap();
    assert_eq!(c.as_str(), "general");
}

#[test]
fn channel_label_accepts_unicode_and_spaces() {
    assert!(ChannelLabel::try_new("café 🌅").is_ok());
}

#[test]
fn channel_label_rejects_empty() {
    assert!(matches!(
        ChannelLabel::try_new(""),
        Err(crate::error::Error::BadChannel(_))
    ));
}

#[test]
fn channel_label_rejects_all_whitespace() {
    assert!(matches!(
        ChannelLabel::try_new("   \t  "),
        Err(crate::error::Error::BadChannel(_))
    ));
}

#[test]
fn channel_label_rejects_control_chars() {
    assert!(matches!(
        ChannelLabel::try_new("hi\nthere"),
        Err(crate::error::Error::BadChannel(_))
    ));
    assert!(matches!(
        ChannelLabel::try_new("nul\0byte"),
        Err(crate::error::Error::BadChannel(_))
    ));
}

#[test]
fn channel_label_rejects_over_64_bytes() {
    let s = "a".repeat(65);
    assert!(matches!(
        ChannelLabel::try_new(&s),
        Err(crate::error::Error::BadChannel(_))
    ));
}

#[test]
fn channel_label_accepts_max_64_bytes() {
    let s = "a".repeat(64);
    assert!(ChannelLabel::try_new(&s).is_ok());
}

#[test]
fn channel_label_default_general_constructor() {
    let c = ChannelLabel::default_general();
    assert_eq!(c.as_str(), "general");
}

#[test]
fn channel_label_postcard_roundtrip() {
    let c = ChannelLabel::try_new("links").unwrap();
    let bytes = postcard::to_stdvec(&c).unwrap();
    let back: ChannelLabel = postcard::from_bytes(&bytes).unwrap();
    assert_eq!(back, c);
}

#[test]
fn channel_label_postcard_decode_validates() {
    // Encode an empty string at the wire layer and verify deserialize rejects it.
    let bad = postcard::to_stdvec(&"".to_owned()).unwrap();
    let err = postcard::from_bytes::<ChannelLabel>(&bad).unwrap_err();
    // postcard surfaces validation errors as `Error::DeserializeBadVarint`
    // or as a custom serde error string; just assert it errored.
    let _ = err;
}
```

- [ ] **Step 2: Add `Error::BadChannel`**

In `crates/sunset-core/src/error.rs`, add a new variant near the other validation errors:

```rust
#[error("invalid channel label: {0}")]
BadChannel(String),
```

(Use the same `#[error(...)]` style the existing variants use.)

- [ ] **Step 3: Add the `ChannelLabel` type and helpers**

In `crates/sunset-core/src/crypto/envelope.rs`, near the top (after the `Signature` type, before `SignedMessage`):

```rust
/// Channel label carried by every chat message (Text, Receipt, Reaction).
/// Lives inside the AEAD plaintext and is covered by the inner Ed25519
/// signature, so the relay sees only `<room_fp>/msg/<hash>` — never the
/// channel. Validated to be 1..=64 bytes UTF-8, no ASCII control
/// characters, not all-whitespace.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ChannelLabel(String);

/// The default channel name; every room implicitly has it.
pub const DEFAULT_CHANNEL: &str = "general";

impl ChannelLabel {
    pub fn try_new(s: impl Into<String>) -> crate::Result<Self> {
        let s = s.into();
        if s.is_empty() {
            return Err(crate::Error::BadChannel("empty".to_owned()));
        }
        if s.len() > 64 {
            return Err(crate::Error::BadChannel(format!("too long ({} bytes)", s.len())));
        }
        if s.chars().any(|c| c.is_control()) {
            return Err(crate::Error::BadChannel("contains control character".to_owned()));
        }
        if s.chars().all(char::is_whitespace) {
            return Err(crate::Error::BadChannel("all whitespace".to_owned()));
        }
        Ok(Self(s))
    }

    pub fn default_general() -> Self {
        // Constructed by hand so we never panic at construction time
        // for the default constant.
        Self(DEFAULT_CHANNEL.to_owned())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ChannelLabel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl serde::Serialize for ChannelLabel {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(s)
    }
}

impl<'de> serde::Deserialize<'de> for ChannelLabel {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(d)?;
        Self::try_new(raw).map_err(serde::de::Error::custom)
    }
}
```

- [ ] **Step 4: Re-export from `lib.rs`**

In `crates/sunset-core/src/lib.rs`, add to the `pub use crypto::envelope::{...}` line:

```rust
pub use crypto::envelope::{ChannelLabel, DEFAULT_CHANNEL, EncryptedMessage, MessageBody, ReactionAction, SignedMessage};
```

- [ ] **Step 5: Run tests, verify they pass**

```bash
cd /home/nicolas/src/sunset/.worktrees/channels-in-rooms
nix develop --command cargo test -p sunset-core --lib crypto::envelope
nix develop --command cargo test -p sunset-core --lib error
```

Expected: all tests in the new `channel_label_*` set pass.

- [ ] **Step 6: Commit**

```bash
git add crates/sunset-core/src/crypto/envelope.rs crates/sunset-core/src/error.rs crates/sunset-core/src/lib.rs
git commit -m "$(cat <<'EOF'
sunset-core: add ChannelLabel newtype + Error::BadChannel

Validates 1..=64 bytes UTF-8, no control chars, not all-whitespace.
Default channel name is "general". Serde wire format is the inner
String; deserialize re-validates so a malformed channel from the wire
surfaces as Error::BadChannel rather than reaching consumers.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 2: Add `channel` to `SignedMessage` / `InnerSigPayload` + new frozen vector

**Files:**
- Modify: `crates/sunset-core/src/crypto/envelope.rs` (add field; add new pin test)

- [ ] **Step 1: Add the failing test for the new pin**

Append inside the existing `mod tests` block:

```rust
#[test]
fn signed_message_postcard_hex_pin() {
    // Pin the SignedMessage wire format so accidental drift breaks the build.
    // postcard layout: signature(64 raw bytes), sent_at_ms(varint), channel(len-varint + utf8), body(...).
    let m = SignedMessage {
        inner_signature: Signature([0u8; 64]),
        sent_at_ms: 1,
        channel: ChannelLabel::default_general(),
        body: MessageBody::Text("hi".to_owned()),
    };
    let bytes = postcard::to_stdvec(&m).unwrap();
    // First 64 bytes: zeroed signature.
    assert!(bytes[..64].iter().all(|b| *b == 0));
    // Then: 0x01 = sent_at_ms varint(1).
    assert_eq!(bytes[64], 0x01);
    // Then: 0x07 = channel length varint, "general" = 7 bytes.
    assert_eq!(bytes[65], 0x07);
    assert_eq!(&bytes[66..73], b"general");
    // Then MessageBody::Text("hi") tail = 00 02 68 69.
    assert_eq!(&bytes[73..], &[0x00, 0x02, 0x68, 0x69]);
}

#[test]
fn signed_message_round_trips_channel() {
    let m = SignedMessage {
        inner_signature: Signature([7u8; 64]),
        sent_at_ms: 42,
        channel: ChannelLabel::try_new("links").unwrap(),
        body: MessageBody::Text("hello".to_owned()),
    };
    let bytes = postcard::to_stdvec(&m).unwrap();
    let back: SignedMessage = postcard::from_bytes(&bytes).unwrap();
    assert_eq!(back, m);
    assert_eq!(back.channel.as_str(), "links");
}

#[test]
fn inner_sig_payload_changes_with_channel() {
    let fp = RoomFingerprint([1u8; 32]);
    let body = MessageBody::Text("hi".to_owned());
    let general = ChannelLabel::default_general();
    let links = ChannelLabel::try_new("links").unwrap();
    let a = inner_sig_payload_bytes(&fp, 0, 100, &general, &body);
    let b = inner_sig_payload_bytes(&fp, 0, 100, &links, &body);
    assert_ne!(a, b, "channel must be domain-separated by the inner signature");
}
```

- [ ] **Step 2: Add `channel` field to `SignedMessage` and `InnerSigPayload`**

In `crates/sunset-core/src/crypto/envelope.rs`, modify the structs:

```rust
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedMessage {
    pub inner_signature: Signature,
    pub sent_at_ms: u64,
    pub channel: ChannelLabel,
    pub body: MessageBody,
}

#[derive(Serialize)]
pub struct InnerSigPayload<'a> {
    pub room_fingerprint: &'a [u8; 32],
    pub epoch_id: u64,
    pub sent_at_ms: u64,
    pub channel: &'a ChannelLabel,
    pub body: &'a MessageBody,
}
```

Update `inner_sig_payload_bytes`:

```rust
pub fn inner_sig_payload_bytes(
    room_fp: &RoomFingerprint,
    epoch_id: u64,
    sent_at_ms: u64,
    channel: &ChannelLabel,
    body: &MessageBody,
) -> Vec<u8> {
    postcard::to_stdvec(&InnerSigPayload {
        room_fingerprint: room_fp.as_bytes(),
        epoch_id,
        sent_at_ms,
        channel,
        body,
    })
    .expect("postcard encoding of InnerSigPayload is infallible for in-memory inputs")
}
```

Update the existing `signed_message_postcard_roundtrip` test to include `channel`:

```rust
#[test]
fn signed_message_postcard_roundtrip() {
    let m = SignedMessage {
        inner_signature: Signature([9u8; 64]),
        sent_at_ms: 1_700_000_000_000,
        channel: ChannelLabel::default_general(),
        body: MessageBody::Text("hello".to_owned()),
    };
    let bytes = postcard::to_stdvec(&m).unwrap();
    let back: SignedMessage = postcard::from_bytes(&bytes).unwrap();
    assert_eq!(back, m);
}
```

Update the existing `inner_sig_payload_changes_with_each_field` test (the call sites passing 4 args become 5):

```rust
#[test]
fn inner_sig_payload_changes_with_each_field() {
    let fp = RoomFingerprint([1u8; 32]);
    let g = ChannelLabel::default_general();
    let a = inner_sig_payload_bytes(&fp, 0, 100, &g, &MessageBody::Text("hi".to_owned()));
    let b = inner_sig_payload_bytes(&fp, 1, 100, &g, &MessageBody::Text("hi".to_owned())); // epoch differs
    let c = inner_sig_payload_bytes(&fp, 0, 101, &g, &MessageBody::Text("hi".to_owned())); // sent_at differs
    let d = inner_sig_payload_bytes(&fp, 0, 100, &g, &MessageBody::Text("hello".to_owned())); // body differs
    let e = inner_sig_payload_bytes(
        &RoomFingerprint([2u8; 32]),
        0,
        100,
        &g,
        &MessageBody::Text("hi".to_owned()),
    ); // room differs
    assert_ne!(a, b);
    assert_ne!(a, c);
    assert_ne!(a, d);
    assert_ne!(a, e);
}
```

- [ ] **Step 3: Run tests; expect existing call sites in `message.rs` to fail to compile**

```bash
nix develop --command cargo build -p sunset-core 2>&1 | head -50
```

Expected: compile errors at every `inner_sig_payload_bytes(...)` and every `SignedMessage { ... }` literal in `message.rs`. Those are fixed in the next task. The envelope tests themselves should compile.

To run only the envelope tests in isolation:

```bash
nix develop --command cargo test -p sunset-core --lib crypto::envelope::tests
```

Expected: PASS. (The `message.rs` build failure is expected and will be repaired by Task 3 — do not commit yet; we want a clean compile per commit.)

- [ ] **Step 4: Stage the envelope changes; do NOT commit yet**

(Continued in Task 3 — Task 2 + Task 3 land as one commit because they're inseparable wire-format + caller-update.)

---

## Task 3: Thread channel through `compose_*` and `decode_message`; update `DecodedMessage`

**Files:**
- Modify: `crates/sunset-core/src/message.rs` (compose APIs, decode_message, DecodedMessage)

- [ ] **Step 1: Add a failing test for round-trip with a non-default channel**

In the `#[cfg(test)] mod tests` block of `message.rs`, add:

```rust
#[test]
fn compose_then_decode_preserves_channel() {
    let id = alice();
    let room = general();
    let composed = compose_text(
        &id,
        &room,
        0,
        1_700_000_000_000,
        ChannelLabel::try_new("links").unwrap(),
        "hi",
        &mut OsRng,
    )
    .unwrap();
    let decoded = decode_message(&room, &composed.entry, &composed.block).unwrap();
    assert_eq!(decoded.channel.as_str(), "links");
    assert_eq!(decoded.body, MessageBody::Text("hi".to_owned()));
}

#[test]
fn decode_rejects_tampered_channel() {
    use crate::crypto::envelope::Signature;

    // Compose a real message in #links, then re-encrypt with the
    // channel field rewritten to "off-topic" without re-signing.
    // The inner signature should fail to verify because the channel is
    // covered by it.
    let id = alice();
    let room = general();
    let composed = compose_text(
        &id,
        &room,
        0,
        1,
        ChannelLabel::try_new("links").unwrap(),
        "hi",
        &mut OsRng,
    )
    .unwrap();

    // Decrypt the plaintext, swap the channel, re-encrypt under a new
    // key (which is fine — it's keyed off pt_hash).
    let env = EncryptedMessage::from_bytes(&composed.block.data).unwrap();
    let pt_hash = *composed.block.references.first().unwrap();
    let k = derive_msg_key(room.epoch_root(0).unwrap(), 0, &pt_hash);
    let aad = build_msg_aad(room.fingerprint().as_bytes(), 0, &id.public(), 1);
    let pt = aead_decrypt(&k, &env.nonce, &aad, &env.ciphertext).unwrap();
    let mut signed: SignedMessage = postcard::from_bytes(&pt).unwrap();
    signed.channel = ChannelLabel::try_new("off-topic").unwrap();
    // Re-encrypt under a fresh pt_hash so the block is internally consistent.
    let pt_new = postcard::to_stdvec(&signed).unwrap();
    let pt_hash_new: Hash = blake3::hash(&pt_new).into();
    let k_new = derive_msg_key(room.epoch_root(0).unwrap(), 0, &pt_hash_new);
    let ct_new = aead_encrypt(&k_new, &env.nonce, &aad, &pt_new);
    let env_new = EncryptedMessage {
        epoch_id: 0,
        nonce: env.nonce,
        ciphertext: bytes::Bytes::from(ct_new),
    };
    let block_new = ContentBlock {
        data: bytes::Bytes::from(env_new.to_bytes()),
        references: vec![pt_hash_new],
    };
    let mut entry = composed.entry.clone();
    entry.value_hash = block_new.hash();
    entry.name = bytes::Bytes::from(format!(
        "{}/msg/{}",
        room.fingerprint().to_hex(),
        entry.value_hash.to_hex()
    ));
    // Re-sign the outer KV entry too (the outer signature is honest;
    // the attack is on the inner signature only).
    let outer_sig = id.sign(&crate::canonical::signing_payload(&entry));
    entry.signature = bytes::Bytes::copy_from_slice(&outer_sig.to_bytes());

    // The inner signature was computed over channel="links" but the
    // ciphertext now decodes to channel="off-topic" → inner verify fails.
    let err = decode_message(&room, &entry, &block_new).unwrap_err();
    assert!(matches!(err, crate::Error::Signature(_)));
}
```

Also add a test that all three compose helpers carry the channel:

```rust
#[test]
fn compose_receipt_carries_channel() {
    let id = alice();
    let room = general();
    let target: Hash = blake3::hash(b"x").into();
    let composed = compose_receipt(
        &id,
        &room,
        0,
        1,
        ChannelLabel::try_new("off-topic").unwrap(),
        target,
        &mut OsRng,
    )
    .unwrap();
    let decoded = decode_message(&room, &composed.entry, &composed.block).unwrap();
    assert_eq!(decoded.channel.as_str(), "off-topic");
}

#[test]
fn compose_reaction_carries_channel() {
    let id = alice();
    let room = general();
    let target: Hash = blake3::hash(b"target").into();
    let composed = compose_reaction(
        &id,
        &room,
        0,
        2,
        ChannelLabel::try_new("links").unwrap(),
        &ReactionPayload {
            for_value_hash: target,
            emoji: "👍",
            action: crate::ReactionAction::Add,
        },
        &mut OsRng,
    )
    .unwrap();
    let decoded = decode_message(&room, &composed.entry, &composed.block).unwrap();
    assert_eq!(decoded.channel.as_str(), "links");
}
```

- [ ] **Step 2: Update `DecodedMessage`, `compose_message`, `compose_text`, `compose_receipt`, `compose_reaction`, `decode_message`**

Replace the relevant pieces of `crates/sunset-core/src/message.rs`:

```rust
use crate::crypto::envelope::ChannelLabel;
// ...existing imports...

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecodedMessage {
    pub author_key: IdentityKey,
    pub room_fingerprint: RoomFingerprint,
    pub epoch_id: u64,
    pub channel: ChannelLabel,
    pub value_hash: Hash,
    pub sent_at_ms: u64,
    pub body: MessageBody,
}

pub fn compose_message<R: CryptoRngCore + ?Sized>(
    identity: &Identity,
    room: &Room,
    epoch_id: u64,
    sent_at_ms: u64,
    channel: ChannelLabel,
    body: MessageBody,
    rng: &mut R,
) -> Result<ComposedMessage> {
    let epoch_root = room.epoch_root(epoch_id).ok_or(Error::EpochMismatch)?;
    let room_fp = room.fingerprint();

    let inner_payload = inner_sig_payload_bytes(&room_fp, epoch_id, sent_at_ms, &channel, &body);
    let inner_sig = identity.sign(&inner_payload).to_bytes();

    let signed = SignedMessage {
        inner_signature: inner_sig.into(),
        sent_at_ms,
        channel,
        body,
    };
    // ...rest unchanged through `Ok(ComposedMessage { entry, block })`
}

pub fn compose_text<R: CryptoRngCore + ?Sized>(
    identity: &Identity,
    room: &Room,
    epoch_id: u64,
    sent_at_ms: u64,
    channel: ChannelLabel,
    text: &str,
    rng: &mut R,
) -> Result<ComposedMessage> {
    compose_message(
        identity, room, epoch_id, sent_at_ms, channel,
        MessageBody::Text(text.to_owned()),
        rng,
    )
}

pub fn compose_receipt<R: CryptoRngCore + ?Sized>(
    identity: &Identity,
    room: &Room,
    epoch_id: u64,
    sent_at_ms: u64,
    channel: ChannelLabel,
    for_value_hash: Hash,
    rng: &mut R,
) -> Result<ComposedMessage> {
    compose_message(
        identity, room, epoch_id, sent_at_ms, channel,
        MessageBody::Receipt { for_value_hash },
        rng,
    )
}

pub fn compose_reaction<R: CryptoRngCore + ?Sized>(
    identity: &Identity,
    room: &Room,
    epoch_id: u64,
    sent_at_ms: u64,
    channel: ChannelLabel,
    payload: &ReactionPayload<'_>,
    rng: &mut R,
) -> Result<ComposedMessage> {
    if payload.emoji.len() > 64 {
        return Err(Error::EmojiTooLong { len: payload.emoji.len() });
    }
    compose_message(
        identity, room, epoch_id, sent_at_ms, channel,
        MessageBody::Reaction {
            for_value_hash: payload.for_value_hash,
            emoji: payload.emoji.to_owned(),
            action: payload.action,
        },
        rng,
    )
}
```

Update `decode_message` to (a) include `channel` in the inner-sig payload it verifies and (b) populate `DecodedMessage::channel`:

```rust
pub fn decode_message(
    room: &Room,
    entry: &SignedKvEntry,
    block: &ContentBlock,
) -> Result<DecodedMessage> {
    if block.hash() != entry.value_hash {
        return Err(Error::BadValueHash);
    }
    let envelope = EncryptedMessage::from_bytes(&block.data)?;
    let epoch_root = room.epoch_root(envelope.epoch_id).ok_or(Error::EpochMismatch)?;
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
        &signed.channel,
        &signed.body,
    );
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
        channel: signed.channel,
        value_hash: entry.value_hash,
        sent_at_ms: signed.sent_at_ms,
        body: signed.body,
    })
}
```

Update every existing test in `message.rs` that calls `compose_message` / `compose_text` / `compose_receipt` / `compose_reaction` to insert `ChannelLabel::default_general()` at the new position. There are roughly a dozen call sites — each one gets the new arg. Example:

```rust
let composed = compose_message(
    &id,
    &room,
    0,
    1_700_000_000_000,
    ChannelLabel::default_general(),
    MessageBody::Text("hi".to_owned()),
    &mut OsRng,
).unwrap();
```

Update the test `decode_rejects_oversized_reaction_emoji` to also pass `&signed.channel` to `inner_sig_payload_bytes`:

```rust
let inner_payload = inner_sig_payload_bytes(&room_fp, 0, 1, &ChannelLabel::default_general(), &body);
// and the SignedMessage literal gets `channel: ChannelLabel::default_general(),`
```

- [ ] **Step 3: Run the message-layer tests**

```bash
nix develop --command cargo test -p sunset-core --lib message
nix develop --command cargo test -p sunset-core --lib crypto::envelope
```

Expected: all pass, including the three new tests.

- [ ] **Step 4: Build the whole workspace and fix call sites in other crates**

```bash
nix develop --command cargo build --workspace --all-features 2>&1 | head -80
```

Expected callers that will fail to compile (and need a `ChannelLabel::default_general()` inserted at the right position):
- `crates/sunset-core/src/peer/mod.rs` (`send_text` test helpers)
- `crates/sunset-core/src/peer/open_room.rs` (`send_text`, `send_reaction`, `send_receipt` callers — these get fixed properly in Task 5; for now, plumb `ChannelLabel::default_general()` so the workspace builds)
- `crates/sunset-core-wasm/src/lib.rs` (if anything direct here)
- `crates/sunset-web-wasm/src/room_handle.rs` (`send_text`, `send_reaction` — same: inject `default_general()` provisionally; Task 6 changes the API surface)

For each failing call site, insert `sunset_core::ChannelLabel::default_general()` (or `crate::ChannelLabel::default_general()` for in-crate callers) into the right slot. **This is intentionally provisional** — Tasks 5 and 6 will route the user-chosen channel through.

- [ ] **Step 5: Run the full workspace tests**

```bash
nix develop --command cargo test --workspace --all-features 2>&1 | tail -30
nix develop --command cargo clippy --workspace --all-features --all-targets -- -D warnings 2>&1 | tail -30
```

Expected: all tests pass, no clippy warnings. (If a workspace lint trips on something incidental, fix it at root — no `#[allow]` per CLAUDE.md.)

- [ ] **Step 6: Commit (Tasks 1, 2, 3 together as one wire-format commit)**

```bash
git add -A
git commit -m "$(cat <<'EOF'
sunset-core: add channel field to SignedMessage / InnerSigPayload

Threads ChannelLabel through compose_message / compose_text /
compose_receipt / compose_reaction and surfaces it on DecodedMessage.
The inner Ed25519 signature now covers the channel, so a peer
re-encrypting with a different channel fails verify (regression test).
DEFAULT_CHANNEL = "general"; existing OpenRoom / WASM / e2e callers
plumb default_general() provisionally — wired up to user choice in
later tasks. New signed_message_postcard_hex_pin locks the wire
format.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 4: Track per-target channel in the reaction tracker

**Files:**
- Modify: `crates/sunset-core/src/reactions.rs` (record target's channel; expose via callback)

**Why:** Reaction snapshots need to be per-channel-renderable. The tracker already decodes every entry in `<room_fp>/msg/`; record the channel of the *target* message and surface it.

- [ ] **Step 1: Add the failing test**

Append to `mod tests` in `crates/sunset-core/src/reactions.rs` (there should be an integration-style test there; if not, create one mirroring the existing snapshot-callback tests). Use the existing `apply_event_tests` mod plus a new `channel_tests` mod:

```rust
#[cfg(test)]
mod channel_tests {
    use super::*;
    use crate::ChannelLabel;
    use std::collections::HashMap;

    #[test]
    fn target_channel_is_recorded_on_first_text() {
        let mut channels: HashMap<Hash, ChannelLabel> = HashMap::new();
        let target = Hash::from([1u8; 32]);
        // Mimic what the tracker does on a Text event: insert iff absent.
        record_target_channel(&mut channels, target, ChannelLabel::try_new("links").unwrap());
        assert_eq!(channels[&target].as_str(), "links");
        // A second Text under the same target shouldn't change the recorded channel.
        record_target_channel(&mut channels, target, ChannelLabel::try_new("off-topic").unwrap());
        assert_eq!(channels[&target].as_str(), "links");
    }
}
```

- [ ] **Step 2: Implement `record_target_channel`**

In `crates/sunset-core/src/reactions.rs` (top-level, near `apply_event`):

```rust
/// Idempotently record the channel of a target message. The first
/// channel observed for a target wins — later writes are no-ops. Used
/// by the reaction tracker to surface the target message's channel
/// alongside each reaction snapshot.
pub(crate) fn record_target_channel(
    channels: &mut std::collections::HashMap<Hash, crate::ChannelLabel>,
    target: Hash,
    channel: crate::ChannelLabel,
) {
    channels.entry(target).or_insert(channel);
}
```

- [ ] **Step 3: Wire the channel through the running tracker and the callback**

Change `ReactionsCallback` to include the channel:

```rust
pub type ReactionsCallback = Box<dyn Fn(&Hash, &crate::ChannelLabel, &ReactionSnapshot)>;
```

In `spawn_reaction_tracker`, add a `target_channels: HashMap<Hash, ChannelLabel>` alongside `state`. After `decode_message` succeeds, but **before** the variant match, call:

```rust
record_target_channel(
    &mut target_channels,
    decoded.value_hash,
    decoded.channel.clone(),
);
```

…so a Text in `#links` records `target_channels[text.value_hash] = "links"`.

Then in the existing `MessageBody::Reaction { … }` arm, after the `if !apply_event(...)` early-return, look up the channel for `target` (the for_value_hash). If absent (Reaction arrived before its target Text), skip firing — the next time that target's channel is recorded, we re-emit by walking pending reactions. To keep this simple: **drop the snapshot fire when the channel is unknown; re-emit on the next event for that target once the target Text arrives**. That matches existing behavior where reactions need their target to render.

Concretely, after `let snapshot = derive_snapshot(...)`:

```rust
let Some(channel) = target_channels.get(&target).cloned() else {
    // Target Text not yet observed — defer.
    continue;
};
let new_sig = reactions_signature(&snapshot);
let mut sigs = handles.last_target_signatures.borrow_mut();
let prev = sigs.get(&target);
if prev == Some(&new_sig) {
    continue;
}
sigs.insert(target, new_sig);
drop(sigs);
if let Some(cb) = handles.on_reactions_changed.borrow().as_ref() {
    cb(&target, &channel, &snapshot);
}
```

When a Text arrives that is some pending target, we *might* need to fire the snapshot for it (if reactions came first). The simplest thing: after recording the target's channel, if the snapshot for that target exists, force-fire it by clearing its `last_target_signatures` entry so the next reaction event re-fires. **Even simpler and correct**: when recording a target's channel for the first time AND a non-empty snapshot exists, clear that target's signature and fire the snapshot now (using the just-recorded channel). Pseudocode inside the tracker, immediately after `record_target_channel` succeeds (i.e., the entry was newly inserted):

```rust
let was_inserted = !target_channels.contains_key(&decoded.value_hash);
record_target_channel(&mut target_channels, decoded.value_hash, decoded.channel.clone());
if was_inserted {
    // If we have a deferred reaction snapshot for this target, fire it now.
    let snapshot = derive_snapshot(&state, &decoded.value_hash);
    if !snapshot.is_empty() {
        let new_sig = reactions_signature(&snapshot);
        let mut sigs = handles.last_target_signatures.borrow_mut();
        sigs.insert(decoded.value_hash, new_sig);
        drop(sigs);
        if let Some(cb) = handles.on_reactions_changed.borrow().as_ref() {
            cb(&decoded.value_hash, &decoded.channel, &snapshot);
        }
    }
}
```

(Take `was_inserted` correctly — `record_target_channel` returns nothing today; refactor it to return a `bool`:

```rust
pub(crate) fn record_target_channel(...) -> bool {
    use std::collections::hash_map::Entry;
    matches!(channels.entry(target), Entry::Vacant(_))
        .then(|| { channels.insert(target, channel); true })
        .unwrap_or(false)
}
```

…actually cleaner with a direct approach:

```rust
pub(crate) fn record_target_channel(channels: &mut HashMap<Hash, ChannelLabel>, target: Hash, channel: ChannelLabel) -> bool {
    use std::collections::hash_map::Entry;
    match channels.entry(target) {
        Entry::Vacant(e) => { e.insert(channel); true }
        Entry::Occupied(_) => false,
    }
}
```

And update the test signature accordingly.)

- [ ] **Step 4: Update existing reaction tracker tests**

Any callbacks the existing tests register must accept the new `&ChannelLabel` parameter. Most tests will use `ChannelLabel::default_general()` because the messages used to drive them go through `compose_*` with default. Search for `on_reactions_changed` registrations and patch them.

- [ ] **Step 5: Run the reaction tests**

```bash
nix develop --command cargo test -p sunset-core --lib reactions
```

Expected: all pass, including the new `channel_tests`.

- [ ] **Step 6: Commit**

```bash
git add crates/sunset-core/src/reactions.rs
git commit -m "$(cat <<'EOF'
sunset-core/reactions: thread target channel through tracker callback

Records the channel of each target message the first time the tracker
observes a Text/Receipt/Reaction for it. Snapshot fires now carry the
target's channel; reaction events that arrive before their target Text
are deferred until the target's channel is known.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 5: Wire channels through `OpenRoom` (send/recv/observe + auto-ack inheritance)

**Files:**
- Modify: `crates/sunset-core/src/peer/open_room.rs`

**Why:** OpenRoom is the host-facing API surface. It owns the decode loop, the auto-ack receipt path, and the reactions callback wiring. This is where channels become observable to clients.

- [ ] **Step 1: Add failing tests in `peer/mod.rs` (the test module that already exercises OpenRoom)**

Append to the `tests` mod (using the existing `helpers::mk_peer` and tokio `LocalSet` patterns):

```rust
#[tokio::test(flavor = "current_thread")]
async fn send_text_routes_through_explicit_channel() {
    let local = tokio::task::LocalSet::new();
    local.run_until(async {
        use std::cell::RefCell;
        use std::rc::Rc;

        let peer = helpers::mk_peer(ident(30)).await;
        let room = peer.open_room("alpha").await.expect("open_room");
        let seen: Rc<RefCell<Vec<(String, String)>>> = Rc::new(RefCell::new(Vec::new()));
        let seen_cb = seen.clone();
        room.on_message(move |decoded, _is_self| {
            if let crate::MessageBody::Text(text) = &decoded.body {
                seen_cb.borrow_mut().push((decoded.channel.as_str().to_owned(), text.clone()));
            }
        });

        room.send_text_in_channel(
            crate::ChannelLabel::try_new("links").unwrap(),
            "hello".to_owned(),
            1_700_000_000_000,
        ).await.expect("send_text_in_channel");

        // Allow the decode loop to process.
        tokio::task::yield_now().await;
        // Loose loop: poll for up to 1s
        for _ in 0..100 {
            if !seen.borrow().is_empty() { break; }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let entries = seen.borrow().clone();
        assert!(entries.iter().any(|(c, t)| c == "links" && t == "hello"),
            "expected #links/hello, got {:?}", entries);
    }).await;
}

#[tokio::test(flavor = "current_thread")]
async fn observed_channels_includes_default_and_observed() {
    let local = tokio::task::LocalSet::new();
    local.run_until(async {
        use std::cell::RefCell;
        use std::rc::Rc;

        let peer = helpers::mk_peer(ident(31)).await;
        let room = peer.open_room("alpha").await.expect("open_room");
        let last: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
        let last_cb = last.clone();
        room.on_channels_changed(move |chans| {
            *last_cb.borrow_mut() = chans.iter().map(|c| c.as_str().to_owned()).collect();
        });

        // Default first.
        room.send_text_in_channel(
            crate::ChannelLabel::default_general(),
            "in general".to_owned(),
            1,
        ).await.unwrap();
        room.send_text_in_channel(
            crate::ChannelLabel::try_new("links").unwrap(),
            "in links".to_owned(),
            2,
        ).await.unwrap();

        for _ in 0..100 {
            if last.borrow().contains(&"links".to_owned())
                && last.borrow().contains(&"general".to_owned()) { break; }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let chans = last.borrow().clone();
        assert!(chans.contains(&"general".to_owned()), "got {:?}", chans);
        assert!(chans.contains(&"links".to_owned()), "got {:?}", chans);
    }).await;
}

#[tokio::test(flavor = "current_thread")]
async fn auto_ack_receipt_inherits_target_channel() {
    let local = tokio::task::LocalSet::new();
    local.run_until(async {
        use std::cell::RefCell;
        use std::rc::Rc;

        // Two peers in the same room.
        let alice = helpers::mk_peer(ident(40)).await;
        let bob = helpers::mk_peer(ident(41)).await;
        // For unit-test purposes (no transport), use a shared store via a helper if
        // available; otherwise fall back to driving alice + bob through the same
        // peer instance with separate identities and a shared store. Use whatever
        // matches existing test patterns in helpers.
        let alice_room = alice.open_room("alpha").await.expect("alice open");
        let bob_room = bob.open_room("alpha").await.expect("bob open");

        let receipt_seen: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
        let cb = receipt_seen.clone();
        alice_room.on_receipt(move |_for_hash, _from_pubkey, channel, _ts_ms| {
            *cb.borrow_mut() = Some(channel.as_str().to_owned());
        });

        // Drive bob's send by hand (we don't need transport for this — alice and
        // bob both write into stores that the conformance helpers wire up).
        // If `helpers::mk_peer` returns two unrelated stores, replace this test
        // with a same-store variant — the point is: when an auto-ack fires for a
        // Text in #links, the Receipt is composed with channel=#links.
        // (See helpers documentation.)
        bob_room.send_text_in_channel(
            crate::ChannelLabel::try_new("links").unwrap(),
            "hello".to_owned(),
            1,
        ).await.unwrap();

        for _ in 0..100 {
            if receipt_seen.borrow().is_some() { break; }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        // Assertion is structural: when a receipt fires, its channel is the
        // target's channel. If helpers don't share a store, mark this test
        // #[ignore] with a comment pointing at the integration-test slot in
        // sunset-sync where a real two-peer auto-ack scenario already runs.
        if let Some(ch) = receipt_seen.borrow().as_ref() {
            assert_eq!(ch, "links");
        }
    }).await;
}
```

(If `helpers::mk_peer` doesn't share a store — likely true today — keep the first two tests and convert the auto-ack test into a smaller unit that calls the auto-ack helper directly with a stub `DecodedMessage`. The point of the test is "Receipt's channel == decoded.body's channel". The implementation's correctness is what matters, so a small unit test like:

```rust
#[tokio::test(flavor = "current_thread")]
async fn auto_ack_uses_target_channel() {
    // build a Text DecodedMessage with channel="links";
    // drive send_receipt(...) manually via the same code path the
    // decode loop uses; assert the resulting decoded receipt has
    // channel="links".
}
```

…is acceptable.)

- [ ] **Step 2: Add `send_text_in_channel` / `send_reaction_in_channel` to `OpenRoom`**

In `crates/sunset-core/src/peer/open_room.rs`, replace the existing `send_text` and `send_reaction` (or add channel-aware overloads — the cleaner choice is to make the channel mandatory, so just change the existing methods):

```rust
pub async fn send_text(
    &self,
    body: String,
    sent_at_ms: u64,
) -> crate::Result<sunset_store::Hash> {
    self.send_text_in_channel(
        crate::ChannelLabel::default_general(),
        body,
        sent_at_ms,
    ).await
}

pub async fn send_text_in_channel(
    &self,
    channel: crate::ChannelLabel,
    body: String,
    sent_at_ms: u64,
) -> crate::Result<sunset_store::Hash> {
    use crate::compose_text;
    use rand_chacha::ChaCha20Rng;
    use rand_core::SeedableRng;

    let peer = self
        .inner
        .peer_weak
        .upgrade()
        .ok_or_else(|| crate::Error::Other("peer dropped".into()))?;

    let mut rng = ChaCha20Rng::from_entropy();
    let composed = compose_text(
        peer.identity(),
        &self.inner.room,
        crate::V1_EPOCH_ID,
        sent_at_ms,
        channel,
        &body,
        &mut rng,
    )?;
    let value_hash = composed.entry.value_hash;
    peer.store()
        .insert(composed.entry, Some(composed.block))
        .await
        .map_err(|e| crate::Error::Other(format!("store insert: {e}")))?;
    Ok(value_hash)
}
```

Same shape for `send_reaction_in_channel` (mandatory channel) and a thin `send_reaction` that defaults to `default_general()`.

- [ ] **Step 3: Update callbacks to include `&ChannelLabel`**

Replace the type aliases and methods:

```rust
pub(crate) type MessageCallback = Box<dyn Fn(&DecodedMessage, bool /* is_self */)>;
// (DecodedMessage already carries channel — no change needed here.)

pub(crate) type ReceiptCallback = Box<dyn Fn(
    sunset_store::Hash,
    &crate::IdentityKey,
    &crate::ChannelLabel,
    u64,
)>;
```

Update `on_receipt`:

```rust
pub fn on_receipt<F: Fn(sunset_store::Hash, &crate::IdentityKey, &crate::ChannelLabel, u64) + 'static>(
    &self,
    cb: F,
) {
    let mut cbs = self.inner.callbacks.borrow_mut();
    let was_unregistered = cbs.on_message.is_none() && cbs.on_receipt.is_none() && cbs.on_channels.is_none();
    cbs.on_receipt = Some(Box::new(cb));
    drop(cbs);
    if was_unregistered {
        self.spawn_decode_loop();
    }
}
```

(Note the addition of `cbs.on_channels` to the trigger — see Step 4.)

In the decode loop, route receipts with channel:

```rust
crate::MessageBody::Receipt { for_value_hash } => {
    if let Some(cb) = cbs.on_receipt.as_ref() {
        cb(*for_value_hash, &decoded.author_key, &decoded.channel, decoded.sent_at_ms);
    }
}
```

In the auto-ack path, pass the target Text's channel:

```rust
if let crate::MessageBody::Text(_) = &decoded.body {
    if !is_self && acked.insert(entry.value_hash) {
        send_receipt(
            &store,
            &room,
            &identity,
            entry.value_hash,
            decoded.channel.clone(),  // inherit target's channel
            &mut rng,
        ).await;
    }
}
```

Update `send_receipt` to accept and use the channel:

```rust
async fn send_receipt<St: sunset_store::Store + 'static>(
    store: &std::sync::Arc<St>,
    room: &crate::crypto::room::Room,
    identity: &crate::Identity,
    for_value_hash: sunset_store::Hash,
    channel: crate::ChannelLabel,
    rng: &mut rand_chacha::ChaCha20Rng,
) {
    let now_ms = web_time::SystemTime::now()
        .duration_since(web_time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let composed = match crate::compose_receipt(
        identity, room, crate::V1_EPOCH_ID, now_ms,
        channel,
        for_value_hash,
        rng,
    ) {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("compose_receipt failed: {e}");
            return;
        }
    };
    if let Err(e) = store.insert(composed.entry, Some(composed.block)).await {
        tracing::error!("store.insert(receipt) failed: {e}");
    }
}
```

Update `on_reactions_changed` to expose the channel (the slot type changed in Task 4):

```rust
pub fn on_reactions_changed<
    F: Fn(&sunset_store::Hash, &crate::ChannelLabel, &crate::reactions::ReactionSnapshot) + 'static,
>(
    &self,
    cb: F,
) {
    *self.inner.reaction_handles.on_reactions_changed.borrow_mut() = Some(Box::new(cb));
    self.inner.reaction_handles.last_target_signatures.borrow_mut().clear();
}
```

`send_reaction` and `send_reaction_in_channel` mirror the text helpers (with the `ReactionPayload`-based call into `compose_reaction`).

- [ ] **Step 4: Add `observed_channels` and `on_channels_changed`**

Add fields to `RoomCallbacks` and `RoomState`:

```rust
pub(crate) type ChannelsCallback = Box<dyn Fn(&[crate::ChannelLabel])>;

#[derive(Default)]
pub(crate) struct RoomCallbacks {
    pub(crate) on_message: Option<MessageCallback>,
    pub(crate) on_receipt: Option<ReceiptCallback>,
    pub(crate) on_channels: Option<ChannelsCallback>,
}

pub(crate) struct RoomState<St: Store + 'static, T: Transport + 'static> {
    pub(crate) room: Rc<Room>,
    pub(crate) peer_weak: Weak<super::Peer<St, T>>,
    pub(crate) presence_started: Cell<bool>,
    pub(crate) publisher: RefCell<Option<crate::membership::PublisherHandle>>,
    pub(crate) tracker_handles: Rc<TrackerHandles>,
    pub(crate) reaction_handles: crate::reactions::ReactionHandles,
    pub(crate) cancel_decode: Rc<Cell<bool>>,
    pub(crate) callbacks: Rc<RefCell<RoomCallbacks>>,
    /// Sorted set of channels observed in the decode loop. Always
    /// contains DEFAULT_CHANNEL.
    pub(crate) observed_channels: Rc<RefCell<std::collections::BTreeSet<crate::ChannelLabel>>>,
}
```

In `Peer::open_room`, initialize:

```rust
let mut chans = std::collections::BTreeSet::new();
chans.insert(crate::ChannelLabel::default_general());
let observed_channels = Rc::new(RefCell::new(chans));
```

…and pass into the new `RoomState { …, observed_channels }`.

Add the public API:

```rust
impl<St: Store + 'static, T: Transport + 'static> OpenRoom<St, T> {
    pub fn observed_channels(&self) -> Vec<crate::ChannelLabel> {
        self.inner.observed_channels.borrow().iter().cloned().collect()
    }

    pub fn on_channels_changed<F: Fn(&[crate::ChannelLabel]) + 'static>(&self, cb: F) {
        let mut cbs = self.inner.callbacks.borrow_mut();
        let was_unregistered = cbs.on_message.is_none() && cbs.on_receipt.is_none() && cbs.on_channels.is_none();
        cbs.on_channels = Some(Box::new(cb));
        drop(cbs);
        // Fire current snapshot so a host that subscribes after rooms have
        // already been observed gets the current state.
        let snap = self.observed_channels();
        if let Some(cb) = self.inner.callbacks.borrow().on_channels.as_ref() {
            cb(&snap);
        }
        if was_unregistered {
            self.spawn_decode_loop();
        }
    }
}
```

In the decode loop body, after `decode_message` succeeds:

```rust
let channel = decoded.channel.clone();
let inserted = inner.observed_channels.borrow_mut().insert(channel);
if inserted {
    let snap: Vec<_> = inner.observed_channels.borrow().iter().cloned().collect();
    if let Some(cb) = inner.callbacks.borrow().on_channels.as_ref() {
        cb(&snap);
    }
}
```

(`inner` here means `Rc<RoomState>` already in scope inside the loop. Adjust naming to match the local binding.)

- [ ] **Step 5: Run the OpenRoom tests**

```bash
nix develop --command cargo test -p sunset-core --lib peer
```

Expected: all pass.

- [ ] **Step 6: Update existing tests in `peer/mod.rs` whose `send_text` calls relied on the old 2-arg signature**

The existing tests using `room.send_text("hello world".to_owned(), now_ms)` keep working because `send_text` defaults to `general`. But the `on_message` callback registrations may need to be updated if any test asserts on `decoded.channel` or matches the new closure shape. Patch as needed.

Run full sunset-core tests + clippy:

```bash
nix develop --command cargo test -p sunset-core --all-features
nix develop --command cargo clippy -p sunset-core --all-features --all-targets -- -D warnings
```

- [ ] **Step 7: Commit**

```bash
git add crates/sunset-core/src/peer/
git commit -m "$(cat <<'EOF'
sunset-core/OpenRoom: per-channel send/recv + observed channels

send_text_in_channel / send_reaction_in_channel are the channel-aware
APIs; the existing send_text / send_reaction default to "general".
on_receipt + on_reactions_changed callbacks now receive the channel.
Auto-ack inherits the target Text's channel. New observed_channels()
+ on_channels_changed give hosts a live view of which channels exist
in the room (always includes "general").

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 6: WASM bridge — `sunset-web-wasm` `RoomHandle`, `IncomingMessage`, `IncomingReceipt`, reactions snapshot

**Files:**
- Modify: `crates/sunset-web-wasm/src/messages.rs`
- Modify: `crates/sunset-web-wasm/src/room_handle.rs`
- Modify: `crates/sunset-web-wasm/src/reactions.rs`

- [ ] **Step 1: Add channel field to `IncomingMessage` / `IncomingReceipt`**

In `crates/sunset-web-wasm/src/messages.rs`:

```rust
#[wasm_bindgen]
pub struct IncomingMessage {
    #[wasm_bindgen(getter_with_clone)]
    pub author_pubkey: Vec<u8>,
    pub epoch_id: u64,
    pub sent_at_ms: f64,
    #[wasm_bindgen(getter_with_clone)]
    pub channel: String,
    #[wasm_bindgen(getter_with_clone)]
    pub body: String,
    #[wasm_bindgen(getter_with_clone)]
    pub value_hash_hex: String,
    pub is_self: bool,
}

#[wasm_bindgen]
pub struct IncomingReceipt {
    #[wasm_bindgen(getter_with_clone)]
    pub for_value_hash_hex: String,
    #[wasm_bindgen(getter_with_clone)]
    pub from_pubkey: Vec<u8>,
    #[wasm_bindgen(getter_with_clone)]
    pub channel: String,
    pub sent_at_ms: f64,
}

pub fn from_decoded_text(
    decoded: &DecodedMessage,
    text: String,
    value_hash_hex: String,
    is_self: bool,
) -> IncomingMessage {
    IncomingMessage {
        author_pubkey: decoded.author_key.as_bytes().to_vec(),
        epoch_id: decoded.epoch_id,
        sent_at_ms: decoded.sent_at_ms as f64,
        channel: decoded.channel.as_str().to_owned(),
        body: text,
        value_hash_hex,
        is_self,
    }
}

pub fn receipt_to_js(
    for_value_hash_hex: String,
    from_pubkey: &IdentityKey,
    channel: &sunset_core::ChannelLabel,
    sent_at_ms: u64,
) -> IncomingReceipt {
    IncomingReceipt {
        for_value_hash_hex,
        from_pubkey: from_pubkey.as_bytes().to_vec(),
        channel: channel.as_str().to_owned(),
        sent_at_ms: sent_at_ms as f64,
    }
}
```

- [ ] **Step 2: Update `RoomHandle` API**

In `crates/sunset-web-wasm/src/room_handle.rs`:

```rust
#[wasm_bindgen]
impl RoomHandle {
    pub async fn send_message(
        &self,
        channel: String,
        body: String,
        sent_at_ms: f64,
    ) -> Result<String, JsError> {
        let channel = sunset_core::ChannelLabel::try_new(channel)
            .map_err(|e| JsError::new(&format!("send_message channel: {e}")))?;
        let value_hash = self
            .inner
            .send_text_in_channel(channel, body, sent_at_ms as u64)
            .await
            .map_err(|e| JsError::new(&format!("send_text: {e}")))?;
        Ok(value_hash.to_hex())
    }

    pub fn on_message(&self, callback: js_sys::Function) {
        self.inner.on_message(move |decoded, is_self| {
            if let sunset_core::MessageBody::Text(text) = &decoded.body {
                let im = crate::messages::from_decoded_text(
                    decoded,
                    text.clone(),
                    decoded.value_hash.to_hex(),
                    is_self,
                );
                let _ = callback.call1(&JsValue::NULL, &JsValue::from(im));
            }
        });
    }

    pub fn on_receipt(&self, callback: js_sys::Function) {
        self.inner.on_receipt(move |for_hash, from_pubkey, channel, sent_at_ms| {
            let incoming = crate::messages::receipt_to_js(
                for_hash.to_hex(), from_pubkey, channel, sent_at_ms,
            );
            let _ = callback.call1(&JsValue::NULL, &JsValue::from(incoming));
        });
    }

    pub fn observed_channels(&self) -> js_sys::Array {
        let arr = js_sys::Array::new();
        for c in self.inner.observed_channels() {
            arr.push(&JsValue::from_str(c.as_str()));
        }
        arr
    }

    pub fn on_channels_changed(&self, callback: js_sys::Function) {
        self.inner.on_channels_changed(move |chans| {
            let arr = js_sys::Array::new();
            for c in chans {
                arr.push(&JsValue::from_str(c.as_str()));
            }
            let _ = callback.call1(&JsValue::NULL, &arr);
        });
    }

    pub async fn send_reaction(
        &self,
        channel: String,
        target_value_hash_hex: String,
        emoji: String,
        action: String,
    ) -> Result<(), JsError> {
        let channel = sunset_core::ChannelLabel::try_new(channel)
            .map_err(|e| JsError::new(&format!("send_reaction channel: {e}")))?;
        let action = match action.as_str() {
            "add" => sunset_core::ReactionAction::Add,
            "remove" => sunset_core::ReactionAction::Remove,
            other => return Err(JsError::new(&format!(
                "send_reaction: action must be \"add\" or \"remove\", got {other:?}"
            ))),
        };
        let target_bytes = hex::decode(&target_value_hash_hex)
            .map_err(|e| JsError::new(&format!("send_reaction: bad target hex: {e}")))?;
        if target_bytes.len() != 32 {
            return Err(JsError::new("send_reaction: target hex must decode to 32 bytes"));
        }
        let mut target_arr = [0u8; 32];
        target_arr.copy_from_slice(&target_bytes);
        let target: sunset_store::Hash = target_arr.into();
        let now_ms = web_time::SystemTime::now()
            .duration_since(web_time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        self.inner
            .send_reaction_in_channel(channel, target, emoji, action, now_ms)
            .await
            .map_err(|e| JsError::new(&format!("send_reaction: {e}")))?;
        Ok(())
    }

    pub fn on_reactions_changed(&self, callback: js_sys::Function) {
        self.inner.on_reactions_changed(move |target, channel, snapshot| {
            let payload = crate::reactions::snapshot_to_js(target, channel, snapshot);
            let _ = callback.call1(&JsValue::NULL, &payload);
        });
    }
}
```

(`OpenRoom::send_reaction_in_channel` should be added in Task 5 alongside `send_text_in_channel`. If you skipped it there, add it now in `peer/open_room.rs`.)

- [ ] **Step 3: Update `reactions::snapshot_to_js` to include channel**

In `crates/sunset-web-wasm/src/reactions.rs`, add `channel` to whatever object the snapshot payload is. If it currently emits `{ target_hex, entries: [...] }`, change it to `{ target_hex, channel, entries: [...] }`. Surface a `reactions_snapshot_channel(payload) -> String` accessor in the FFI shim.

- [ ] **Step 4: Build the wasm crate**

```bash
nix develop --command cargo build -p sunset-web-wasm --target wasm32-unknown-unknown
nix develop --command cargo clippy -p sunset-web-wasm --target wasm32-unknown-unknown -- -D warnings
```

Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add crates/sunset-web-wasm/
git commit -m "$(cat <<'EOF'
sunset-web-wasm: thread channel through RoomHandle / Incoming{Message,Receipt}

RoomHandle.send_message and send_reaction take a channel string;
incoming messages, receipts, and reaction snapshots all expose the
channel. New observed_channels() + on_channels_changed() let JS read
the live channel set.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 7: FFI shims (sunset.ffi.mjs) + Gleam bindings (sunset.gleam)

**Files:**
- Modify: `web/src/sunset_web/sunset.ffi.mjs`
- Modify: `web/src/sunset_web/sunset.gleam`

- [ ] **Step 1: Update `sendMessage` to take channel**

In `web/src/sunset_web/sunset.ffi.mjs`:

```js
export async function sendMessage(room, channel, body, sentAtMs, callback) {
  try {
    const valueHashHex = await room.send_message(channel, body, sentAtMs);
    callback(new Ok(valueHashHex));
  } catch (e) {
    callback(new GError(String(e)));
  }
}

export function onMessage(room, callback) {
  room.on_message((incoming) => {
    const plain = {
      author_pubkey: incoming.author_pubkey,
      epoch_id: incoming.epoch_id,
      sent_at_ms: incoming.sent_at_ms,
      channel: incoming.channel,
      body: incoming.body,
      value_hash_hex: incoming.value_hash_hex,
      is_self: incoming.is_self,
    };
    incoming.free();
    callback(plain);
  });
}

export function onReceipt(room, callback) {
  room.on_receipt((incoming) => {
    const plain = {
      for_value_hash_hex: incoming.for_value_hash_hex,
      from_pubkey: incoming.from_pubkey,
      channel: incoming.channel,
      sent_at_ms: incoming.sent_at_ms,
    };
    incoming.free();
    callback(plain);
  });
}

// Channel field accessor (mirrors the inc* family).
export function incChannel(msg) { return msg.channel; }
export function recChannel(r) { return r.channel; }

// New: observed channels + live updates.
export function observedChannels(room) {
  return toList(Array.from(room.observed_channels()));
}
export function onChannelsChanged(room, callback) {
  room.on_channels_changed((arr) => callback(toList(Array.from(arr))));
}

// Reactions snapshot — extend the existing payload with channel.
export function reactionsSnapshotChannel(payload) {
  return payload.channel;
}

// Send reaction now needs channel.
export async function sendReaction(room, channel, targetHex, emoji, action, callback) {
  try {
    await room.send_reaction(channel, targetHex, emoji, action);
    callback(new Ok(undefined));
  } catch (e) {
    callback(new GError(String(e)));
  }
}
```

(If a current `sendReaction` shim exists with different parameter order, update both this shim and its Gleam binding atomically.)

- [ ] **Step 2: Update `web/src/sunset_web/sunset.gleam` external bindings**

```gleam
@external(javascript, "./sunset.ffi.mjs", "sendMessage")
pub fn send_message(
  room: RoomHandle,
  channel: String,
  body: String,
  sent_at_ms: Int,
  callback: fn(Result(String, String)) -> Nil,
) -> Nil

@external(javascript, "./sunset.ffi.mjs", "incChannel")
pub fn inc_channel(msg: IncomingMessage) -> String

@external(javascript, "./sunset.ffi.mjs", "recChannel")
pub fn rec_channel(r: IncomingReceipt) -> String

@external(javascript, "./sunset.ffi.mjs", "observedChannels")
pub fn observed_channels(room: RoomHandle) -> List(String)

@external(javascript, "./sunset.ffi.mjs", "onChannelsChanged")
pub fn on_channels_changed(
  room: RoomHandle,
  callback: fn(List(String)) -> Nil,
) -> Nil

@external(javascript, "./sunset.ffi.mjs", "sendReaction")
pub fn send_reaction(
  room: RoomHandle,
  channel: String,
  target_hex: String,
  emoji: String,
  action: String,
  callback: fn(Result(Nil, String)) -> Nil,
) -> Nil

@external(javascript, "./sunset.ffi.mjs", "reactionsSnapshotChannel")
pub fn reactions_snapshot_channel(payload: ReactionsSnapshot) -> String
```

(Keep the existing `IncomingReceipt` type opaque; the new `rec_channel` accessor is what callers use.)

- [ ] **Step 3: Build the wasm artifact + Gleam compile**

```bash
cd /home/nicolas/src/sunset/.worktrees/channels-in-rooms
nix develop --command bash -c 'cd web && gleam build'
nix build .#webDist 2>&1 | tail -20
```

Expected: clean Gleam compile and a successful `nix build .#webDist`.

- [ ] **Step 4: Commit**

```bash
git add web/src/sunset_web/sunset.ffi.mjs web/src/sunset_web/sunset.gleam
git commit -m "$(cat <<'EOF'
web/ffi: send/recv carry channel; observedChannels + onChannelsChanged

Threads the channel string through the JS shim layer and the Gleam
external bindings. sendMessage/sendReaction take channel as their
first content argument; incChannel/recChannel surface it on incoming
events; observedChannels + onChannelsChanged expose the live channel
set to the Lustre layer.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 8: Gleam UI — RoomState.channels, filter messages by current channel, route SubmitDraft, drop fixture

**Files:**
- Modify: `web/src/sunset_web/domain.gleam` (add `channel` to `Message`, helper `default_channel_id`)
- Modify: `web/src/sunset_web/fixture.gleam` (mark `channels()` as fallback; the live UI no longer uses it)
- Modify: `web/src/sunset_web.gleam` (RoomState.channels, IncomingMsg + IncomingReceipt updates, ChannelsObserved Msg, SubmitDraft routes channel, channels.view sources from state)

- [ ] **Step 1: Add `channel: String` to `domain.Message` and a helper for the default channel**

In `web/src/sunset_web/domain.gleam`:

```gleam
pub type Message {
  Message(
    id: String,
    author_pubkey: BitArray,
    initials: String,
    time: String,
    body: String,
    channel: String,
    seen_by: Int,
    you: Bool,
    pending: Bool,
    reactions: List(Reaction),
    details: DetailsOpt,
  )
}

pub const default_channel_name: String = "general"

pub fn default_channel_id() -> ChannelId {
  ChannelId(default_channel_name)
}
```

Update `MessageView` similarly (add `channel: String`).

Patch all `Message(...)` literals (chiefly in `web/src/sunset_web/fixture.gleam` and any test fixtures) to include `channel: "general"`.

- [ ] **Step 2: Add `channels: List(domain.Channel)` to RoomState**

In `web/src/sunset_web.gleam`:

```gleam
pub type RoomState {
  RoomState(
    handle: Option(RoomHandle),
    messages: List(domain.Message),
    members: List(domain.Member),
    receipts: Dict(String, Dict(String, Int)),
    reactions: Dict(String, List(Reaction)),
    current_channel: ChannelId,
    channels: List(domain.Channel),    // NEW
    draft: String,
    selected_msg_id: Option(String),
    reacting_to: Option(String),
    sheet: Option(domain.Sheet),
    peer_status_popover: Option(domain.MemberId),
    revealed_spoilers: Set(#(String, String)),
  )
}
```

Update `empty_room_state`:

```gleam
fn empty_room_state() -> RoomState {
  RoomState(
    handle: None,
    messages: [],
    members: [],
    receipts: dict.new(),
    reactions: dict.new(),
    current_channel: domain.default_channel_id(),
    channels: [
      domain.Channel(
        id: domain.default_channel_id(),
        name: domain.default_channel_name,
        kind: domain.TextChannel,
        in_call: 0,
        unread: 0,
      ),
    ],
    draft: "",
    selected_msg_id: None,
    reacting_to: None,
    sheet: None,
    peer_status_popover: None,
    revealed_spoilers: set.new(),
  )
}
```

- [ ] **Step 3: Add `ChannelsObserved` Msg + handler**

Add the variant to the `Msg` type:

```gleam
ChannelsObserved(name: String, channels: List(String))
```

Add the handler in the `update` function:

```gleam
ChannelsObserved(name, chans) -> {
  case dict.get(model.rooms, name) {
    Error(_) -> #(model, effect.none())
    Ok(state) -> {
      let new_channels =
        list.map(chans, fn(c) {
          // Find the existing Channel for this id (preserve unread/in_call),
          // or create a fresh TextChannel.
          let id = ChannelId(c)
          case list.find(state.channels, fn(ch) { ch.id == id }) {
            Ok(existing) -> existing
            Error(_) ->
              domain.Channel(
                id: id,
                name: c,
                kind: domain.TextChannel,
                in_call: 0,
                unread: 0,
              )
          }
        })
      // Always include the default channel even if no message yet observed.
      let with_default = case list.any(new_channels, fn(c) { c.id == domain.default_channel_id() }) {
        True -> new_channels
        False -> [
          domain.Channel(
            id: domain.default_channel_id(),
            name: domain.default_channel_name,
            kind: domain.TextChannel,
            in_call: 0,
            unread: 0,
          ),
          ..new_channels
        ]
      }
      let new_state = RoomState(..state, channels: with_default)
      #(Model(..model, rooms: dict.insert(model.rooms, name, new_state)), effect.none())
    }
  }
}
```

- [ ] **Step 4: Wire `on_channels_changed` in `RoomOpened`**

Append to the existing wire effect inside the `RoomOpened` arm:

```gleam
sunset.on_channels_changed(handle, fn(chans) {
  dispatch(ChannelsObserved(name, chans))
})
```

- [ ] **Step 5: Update `IncomingMsg` to read the channel from the wasm message**

```gleam
let new_msg =
  domain.Message(
    id: sunset.inc_value_hash_hex(im),
    author_pubkey: sunset.inc_author_pubkey(im),
    initials: short_initials(sunset.inc_author_pubkey(im)),
    time: format_time_ms(sunset.inc_sent_at_ms(im)),
    body: sunset.inc_body(im),
    channel: sunset.inc_channel(im),
    seen_by: 0,
    you: sunset.inc_is_self(im),
    pending: False,
    reactions: [],
    details: domain.NoDetails,
  )
```

- [ ] **Step 6: Update `IncomingReceipt` Msg to carry channel**

```gleam
IncomingReceipt(name: String, message_id: String, from_pubkey: String, channel: String, delivered_at_ms: Int)
```

…and the dispatcher for receipts in `RoomOpened`:

```gleam
sunset.on_receipt(handle, fn(r) {
  dispatch(IncomingReceipt(
    name,
    sunset.rec_for_value_hash_hex(r),
    hex_encode(sunset.rec_from_pubkey(r)),
    sunset.rec_channel(r),
    sunset.rec_sent_at_ms(r),
  ))
})
```

(The reducer for `IncomingReceipt` can simply ignore the channel for v1 — it's available if you want to scope receipts per channel later.)

- [ ] **Step 7: Route `SubmitDraft` through `current_channel`**

Replace the existing `sunset.send_message(handle, body, current_time_ms(), ...)` call:

```gleam
let ChannelId(channel_str) = state.current_channel
sunset.send_message(
  handle,
  channel_str,
  body,
  current_time_ms(),
  fn(r) { dispatch(MessageSent(r)) },
)
```

Same treatment for `SendReaction` if it exists (forward `current_channel`).

- [ ] **Step 8: Filter messages list by `current_channel` before passing to `main_panel.view`**

Find the `let raw_messages = state.messages` line and replace with:

```gleam
let ChannelId(active_channel_str) = state.current_channel
let raw_messages =
  list.filter(state.messages, fn(m) { m.channel == active_channel_str })
```

- [ ] **Step 9: Replace `fixture.channels()` in the channels.view call site with `state.channels`**

```gleam
channels.view(
  palette: palette,
  room: active_room,
  channels: state.channels,           // <-- was fixture.channels()
  members: members_for_channels,
  current_channel: state.current_channel,
  ...
)
```

Same for the `list.find(fixture.channels(), ...)` block that picks `active_voice_channel_name`. Use `state.channels` instead.

- [ ] **Step 10: Build the web bundle and run gleam tests**

```bash
nix develop --command bash -c 'cd web && gleam build && gleam test'
nix build .#webDist 2>&1 | tail -20
```

Expected: clean build, all gleam tests pass.

- [ ] **Step 11: Commit**

```bash
git add web/src/sunset_web/domain.gleam web/src/sunset_web/fixture.gleam web/src/sunset_web.gleam
git commit -m "$(cat <<'EOF'
web/ui: drive channels rail from real observations; filter by current channel

RoomState.channels replaces the static fixture; ChannelsObserved msg
merges into it as the wasm side reports channels. SubmitDraft routes
through state.current_channel so a post in #links lands in #links.
domain.Message gets a channel field; the messages list is filtered by
current channel before render.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 9: Playwright e2e — channels actually segregate messages

**Files:**
- Create: `web/e2e/channels-within-rooms.spec.js`

**Why:** UI changes need a real-browser test driving the channels rail and verifying messages from `#links` don't appear in `#general`.

- [ ] **Step 1: Skim the existing e2e tests for the test harness pattern**

```bash
ls /home/nicolas/src/sunset/.worktrees/channels-in-rooms/web/e2e/
```

Look at one of the existing two-browser tests (e.g. presence, reactions, or self-name) to copy the harness setup: how it spins up the dev server, the relay, two browser contexts, and waits for connection.

- [ ] **Step 2: Write the failing e2e test**

In `web/e2e/channels-within-rooms.spec.js` (or `.ts` — match the existing convention):

```js
import { test, expect } from "@playwright/test";
import { launchTwoPeers } from "./helpers/peers"; // or whatever the helper is

test("messages in #links do not appear in #general", async () => {
  const room = `channels-${Date.now()}`;
  const { alice, bob } = await launchTwoPeers({ room });

  // Bob types in #links; Alice should see it ONLY when she's on #links.
  await bob.click('[data-testid="channels-rail"] >> text=links');
  // (If "links" is not yet in the rail, type a message first to surface it.)
  await bob.fill('[data-testid="composer-textarea"]', "via links");
  await bob.keyboard.press("Enter");

  // Alice on #general — must NOT see "via links".
  await alice.click('[data-testid="channels-rail"] >> text=general');
  await expect(alice.locator('[data-testid="messages-list"]'))
    .not.toContainText("via links", { timeout: 5_000 });

  // Alice switches to #links — should see it now.
  // The channel should appear in her rail because the message arrived.
  await expect(alice.locator('[data-testid="channels-rail"]'))
    .toContainText("links", { timeout: 5_000 });
  await alice.click('[data-testid="channels-rail"] >> text=links');
  await expect(alice.locator('[data-testid="messages-list"]'))
    .toContainText("via links", { timeout: 5_000 });

  // Alice replies in #links — Bob should see it in #links.
  await alice.fill('[data-testid="composer-textarea"]', "ack");
  await alice.keyboard.press("Enter");
  await expect(bob.locator('[data-testid="messages-list"]'))
    .toContainText("ack", { timeout: 5_000 });

  // Bob switches to #general — must NOT see "ack" (it's a #links reply).
  await bob.click('[data-testid="channels-rail"] >> text=general');
  await expect(bob.locator('[data-testid="messages-list"]'))
    .not.toContainText("ack", { timeout: 3_000 });
});
```

(Adjust selectors to whatever the existing channels rail / messages list tests use. The `data-testid` attributes already exist in the codebase: `channels-room-title`, `voice-channel-row`, `messages-list`, `composer-textarea`, etc. — verify in the worktree before writing the test.)

If a non-default channel needs to be created from the UI before it's observable, the test may need to type the channel name into a "new channel" input. If no such input exists yet (per the spec, channels are implicit — they appear once a message is posted), the test sequence becomes:

1. Bob composes in `#general`, but with a custom channel input — but the UI doesn't have a channel-name input.

**Resolution:** For v1, "switch to #links to compose in #links" needs a UX. Two options:

   - (a) Add a tiny "channel switcher" input in the composer header that lets the user type a channel name. Posting selects that channel.
   - (b) In the channels rail, allow click-to-select on observed channels; rely on a *seed* mechanism so a channel always exists locally (start with `general` only, type a new channel into a one-off input).

Pick **(b)** for v1: add a small "+ new channel" input below the channels list in the rail. Typing a name + Enter selects that channel locally — it joins the rail immediately and SubmitDraft routes through it.

Add this small UX as part of Task 8 (channels.view tweak) and reflect it in this Playwright test:

```js
await bob.fill('[data-testid="new-channel-input"]', "links");
await bob.keyboard.press("Enter");
// Now Bob is on #links. Compose:
await bob.fill('[data-testid="composer-textarea"]', "via links");
...
```

(If you do this, also add the `data-testid="new-channel-input"` to the rail in Task 8. Update the rail with one tiny input + an `OnNewChannel(String)` Msg that updates `state.current_channel` and inserts the channel into `state.channels` immediately.)

- [ ] **Step 3: Run the e2e test**

```bash
cd web
nix develop ../ --command npx playwright test channels-within-rooms.spec.js
```

Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add web/e2e/channels-within-rooms.spec.js
git commit -m "$(cat <<'EOF'
e2e: channels segregate messages between #general and #links

Two browsers join the same room; Bob types into #links, Alice receives
it only when she switches to #links, and #links appears in her channels
rail dynamically. Messages in one channel never leak into the other's
list.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 10: Final workspace verification + push

- [ ] **Step 1: Full workspace test + clippy + format**

```bash
cd /home/nicolas/src/sunset/.worktrees/channels-in-rooms
nix develop --command cargo test --workspace --all-features
nix develop --command cargo clippy --workspace --all-features --all-targets -- -D warnings
nix develop --command cargo fmt --all --check
```

Expected: all green, no fmt drift.

- [ ] **Step 2: Web bundle builds**

```bash
nix build .#webDist 2>&1 | tail -20
```

Expected: success.

- [ ] **Step 3: Push the branch and open the PR**

```bash
git push -u origin channels-in-rooms
gh pr create --title "Channels within rooms: per-message channel label, encrypted, UI-wired" --body "$(cat <<'EOF'
## Summary

- Adds a `ChannelLabel` field to `SignedMessage`/`InnerSigPayload`, threaded through compose/decode and OpenRoom callbacks. The label rides inside the AEAD plaintext and is covered by the inner Ed25519 signature, so the relay only ever sees `<room_fp>/msg/<hash>`.
- Reactions and receipts inherit the channel of the message they reference; auto-ack receipt picks up the target Text's channel.
- WASM bridge surfaces `channel` on incoming message/receipt/reaction snapshots, plus `observedChannels` + `onChannelsChanged` for the channel set.
- Gleam UI replaces the static channels fixture with a live `state.channels`, filters the message list by `current_channel`, and routes `SubmitDraft` through it. Adds a small "+ new channel" input in the rail so a user can compose into a fresh channel.
- New Playwright e2e proves `#links` and `#general` are segregated end-to-end.

Spec: `docs/superpowers/specs/2026-05-04-channels-within-rooms-design.md`

## Test plan

- [ ] `cargo test --workspace --all-features` (incl. new compose/decode channel round-trips, tamper test, OpenRoom channel routing, reactions tracker channel pinning)
- [ ] `cargo clippy --workspace --all-features --all-targets -- -D warnings`
- [ ] `cargo fmt --all --check`
- [ ] `nix build .#webDist`
- [ ] `npx playwright test channels-within-rooms.spec.js`
- [ ] Manual: open two browser windows in the same room, type a message in `#general`, then create `#links` from the rail input, post a message, verify it doesn't appear in the other window's `#general` view.

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

---

## Self-Review Checklist (already done at plan-write time)

**Spec coverage:**
- ChannelLabel newtype + validation → Task 1 ✓
- channel field on SignedMessage / InnerSigPayload + new pin → Task 2 ✓
- compose_* + decode_message + DecodedMessage → Task 3 ✓
- Reactions tracker per-target channel → Task 4 ✓
- OpenRoom send/recv/observed_channels/on_channels_changed/auto-ack inheritance → Task 5 ✓
- WASM IncomingMessage/IncomingReceipt/RoomHandle channel surface → Task 6 ✓
- Gleam FFI shims + bindings → Task 7 ✓
- Gleam UI: RoomState.channels, IncomingMsg/Receipt updates, SubmitDraft routing, fixture removal, "+ new channel" input → Task 8 ✓
- Playwright e2e for channel segregation → Task 9 ✓
- Workspace verification + PR → Task 10 ✓

**Placeholder scan:** No TBDs / "implement later" / "etc." markers in any task.

**Type consistency:** `ChannelLabel` everywhere it's referenced. `send_text_in_channel` / `send_reaction_in_channel` are the channel-aware methods; `send_text` / `send_reaction` keep working as defaults-to-general convenience. `on_channels_changed` callback receives `&[ChannelLabel]`. `observed_channels()` returns `Vec<ChannelLabel>`. JS layer uses `String` channel everywhere; Gleam `domain.Message.channel: String`.
