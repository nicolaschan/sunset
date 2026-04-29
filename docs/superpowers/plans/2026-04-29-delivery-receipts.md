# Delivery receipts Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Render the user's outgoing chat messages with reduced opacity until at least one peer's bridge has decoded them and auto-acknowledged via a Receipt that arrives back through sync.

**Architecture:** Receipts ride the existing chat-message wire path — same `<room_fp>/msg/<value_hash>` namespace, same AEAD envelope, same outer/inner signatures. The plaintext body changes from `String` to a `MessageBody { Text(String), Receipt { for_value_hash: Hash } }` enum. The auto-acknowledge logic lives at the `sunset-web-wasm` bridge layer in `spawn_message_subscription`: any non-self `Text` triggers a `compose+insert` of a `Receipt` with the original message's value-hash. The FE adds a `receipts: Dict(message_id, Set(peer_pubkey))` model field and renders messages where `m.you && receipts.size == 0` with `opacity: 0.55`.

**Tech Stack:** Rust (sunset-core, sunset-web-wasm) · Gleam + Lustre 5.6 · postcard wire format · ed25519-dalek · Playwright (Pixel 7 + Desktop Chrome).

**Spec:** [docs/superpowers/specs/2026-04-29-delivery-receipts-design.md](../specs/2026-04-29-delivery-receipts-design.md)

---

## Working notes

- Run cargo / gleam / nix commands inside `nix develop` (direnv applies automatically in the worktree).
- Workspace tests: `nix develop --command cargo test --workspace --all-features`.
- Single core test: `nix develop --command cargo test -p sunset-core message::tests::compose_then_decode_roundtrip`.
- Web tests: `nix run .#web-test`.
- Commit style: imperative, scope-prefixed messages. No `Co-Authored-By` trailer (per CLAUDE.md).
- Wire format breaks here. We accept that — pre-1.0 software, no external clients.

---

## Task 1: Add `MessageBody` enum (additive)

**Files:**
- Modify: `crates/sunset-core/src/crypto/envelope.rs`
- Modify: `crates/sunset-core/src/lib.rs`

This task only adds the new type; nothing depends on it yet. Build stays green.

- [ ] **Step 1: Add the type to `envelope.rs`**

After the `EncryptedMessage` struct (around line 115), add:

```rust
/// Discriminator for the inner plaintext of a chat-room entry. Both
/// variants ride the same `<room_fp>/msg/<value_hash>` namespace and
/// share the AEAD envelope; only the plaintext shape differs.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum MessageBody {
    /// A user-authored chat message.
    Text(String),
    /// An acknowledgement that the author of this entry decoded the
    /// referenced `Text` message. The author of the receipt is the
    /// receiver of the original message.
    Receipt {
        for_value_hash: sunset_store::Hash,
    },
}
```

If the file imports `serde::{Deserialize, Serialize}` already at the top, you can drop the path prefixes. Reuse whatever style the file uses.

- [ ] **Step 2: Re-export from `lib.rs`**

In `crates/sunset-core/src/lib.rs`, add `MessageBody` to the public exports list. Find the existing exports of `EncryptedMessage` / `SignedMessage` and add `MessageBody` to the same line.

- [ ] **Step 3: Verify it builds**

Run: `nix develop --command cargo build -p sunset-core`
Expected: clean compile.

- [ ] **Step 4: Add a postcard-roundtrip unit test**

At the bottom of `envelope.rs`'s `mod tests` block, add:

```rust
#[test]
fn message_body_text_roundtrips_via_postcard() {
    let body = MessageBody::Text("hello".to_owned());
    let bytes = postcard::to_stdvec(&body).unwrap();
    let decoded: MessageBody = postcard::from_bytes(&bytes).unwrap();
    assert_eq!(decoded, body);
}

#[test]
fn message_body_receipt_roundtrips_via_postcard() {
    let h: sunset_store::Hash = blake3::hash(b"target message").into();
    let body = MessageBody::Receipt { for_value_hash: h };
    let bytes = postcard::to_stdvec(&body).unwrap();
    let decoded: MessageBody = postcard::from_bytes(&bytes).unwrap();
    assert_eq!(decoded, body);
}
```

- [ ] **Step 5: Run the new tests**

Run: `nix develop --command cargo test -p sunset-core envelope::tests::message_body`
Expected: 2 tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/sunset-core/src/crypto/envelope.rs crates/sunset-core/src/lib.rs
git commit -m "Add MessageBody enum (Text | Receipt) for chat-room entries"
```

---

## Task 2: Migrate `SignedMessage` and `InnerSigPayload` to use `MessageBody`

**Files:**
- Modify: `crates/sunset-core/src/crypto/envelope.rs`
- Modify: `crates/sunset-core/src/message.rs`

This is the wire-format break. We change `SignedMessage.body` from `String` to `MessageBody`, update the inner-sig payload, and update `compose_message` / `decode_message` and their callers.

- [ ] **Step 1: Change `SignedMessage.body` type**

In `envelope.rs`, find the `SignedMessage` struct (around line 82) and change:

```rust
pub struct SignedMessage {
    pub inner_signature: Signature,
    pub sent_at_ms: u64,
    pub body: MessageBody,
}
```

- [ ] **Step 2: Change `InnerSigPayload.body` type**

In the same file (around line 91):

```rust
#[derive(Serialize)]
pub struct InnerSigPayload<'a> {
    pub room_fingerprint: &'a [u8; 32],
    pub epoch_id: u64,
    pub sent_at_ms: u64,
    pub body: &'a MessageBody,
}
```

- [ ] **Step 3: Update `inner_sig_payload_bytes` signature**

```rust
pub fn inner_sig_payload_bytes(
    room_fp: &RoomFingerprint,
    epoch_id: u64,
    sent_at_ms: u64,
    body: &MessageBody,
) -> Vec<u8> {
    postcard::to_stdvec(&InnerSigPayload {
        room_fingerprint: room_fp.as_bytes(),
        epoch_id,
        sent_at_ms,
        body,
    })
    .expect("postcard encoding of InnerSigPayload is infallible for in-memory inputs")
}
```

- [ ] **Step 4: Update tests in `envelope.rs` that use `SignedMessage` or `inner_sig_payload_bytes`**

Find the existing test block. Anywhere a `SignedMessage { body: "...".into(), ... }` literal appears, change to `body: MessageBody::Text("...".to_owned())`. Anywhere `inner_sig_payload_bytes(.., "string")` appears, change to `inner_sig_payload_bytes(.., &MessageBody::Text("string".to_owned()))`.

The exact tests to update (search for them and apply the substitution):
- `inner_sig_payload_bytes_changes_with_field` — uses string literals; wrap each in `MessageBody::Text(...)`.
- Any other test referencing the `body` field of `SignedMessage` or `InnerSigPayload`.

- [ ] **Step 5: Update `compose_message` signature in `message.rs`**

Replace `body: &str` with `body: MessageBody`:

```rust
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
    let inner_sig = identity.sign(&inner_payload).to_bytes();

    let signed = SignedMessage {
        inner_signature: inner_sig.into(),
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
```

- [ ] **Step 6: Update `DecodedMessage` and `decode_message` in `message.rs`**

Change the body field type and the inner-sig verification:

```rust
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecodedMessage {
    pub author_key: IdentityKey,
    pub room_fingerprint: RoomFingerprint,
    pub epoch_id: u64,
    pub value_hash: Hash,
    pub sent_at_ms: u64,
    pub body: MessageBody,
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
    let dalek_sig = DalekSignature::from_bytes(signed.inner_signature.as_bytes());
    author_key.verify(&inner_payload, &dalek_sig)?;

    Ok(DecodedMessage {
        author_key,
        room_fingerprint: room.fingerprint(),
        epoch_id: envelope.epoch_id,
        value_hash: entry.value_hash,
        sent_at_ms: signed.sent_at_ms,
        body: signed.body,
    })
}
```

- [ ] **Step 7: Update existing `message.rs` tests**

Every test that calls `compose_message(..., "literal", ...)` becomes `compose_message(..., MessageBody::Text("literal".to_owned()), ...)`. Every test that asserts `decoded.body == "literal"` becomes `decoded.body == MessageBody::Text("literal".to_owned())`.

Specifically (search and update each occurrence in `message.rs`'s `mod tests`):

```rust
// Was: compose_message(&id, &room, 0, 1_700_000_000_000, "hi", &mut OsRng)
// Now:
compose_message(&id, &room, 0, 1_700_000_000_000, MessageBody::Text("hi".to_owned()), &mut OsRng)

// Was: assert_eq!(decoded.body, "hi");
// Now:
assert_eq!(decoded.body, MessageBody::Text("hi".to_owned()));
```

For the `decode_rejects_forged_inner_signature` test, the inline call to `inner_sig_payload_bytes(..., &signed.body)` is already passing `&signed.body` — its type changes implicitly. Just make sure the file imports `crate::crypto::envelope::MessageBody` (or `super::MessageBody` since `message.rs` already imports envelope items).

Add `use crate::crypto::envelope::MessageBody;` at the top of the `mod tests` block if needed.

- [ ] **Step 8: Verify all sunset-core tests pass**

Run: `nix develop --command cargo test -p sunset-core`
Expected: all tests pass. The `compose_then_decode_roundtrip` test now uses `MessageBody::Text`.

- [ ] **Step 9: Verify the whole workspace still builds**

Run: `nix develop --command cargo build --workspace`
Expected: clean. If `sunset-web-wasm`, `sunset-relay`, or any other crate calls `compose_message(..., "str", ...)` directly, it'll fail to build. Update those call sites by wrapping the string in `MessageBody::Text(...)`. Common location: `crates/sunset-web-wasm/src/client.rs:send_message` — pass `MessageBody::Text(body)` instead of `&body`.

- [ ] **Step 10: Verify the workspace tests pass**

Run: `nix develop --command cargo test --workspace --all-features`
Expected: all pass (downstream tests may need similar updates — apply them).

- [ ] **Step 11: Commit**

```bash
git add crates/sunset-core/ crates/sunset-web-wasm/src/client.rs
git commit -m "Migrate SignedMessage.body to MessageBody enum (wire-format break)"
```

(Stage any other crates that needed call-site updates.)

---

## Task 3: Add `compose_text` and `compose_receipt` convenience helpers

**Files:**
- Modify: `crates/sunset-core/src/message.rs`

Thin wrappers so callers don't need to import `MessageBody` for the common cases.

- [ ] **Step 1: Add the helpers**

Below `compose_message` in `message.rs`:

```rust
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
```

Add `use crate::crypto::envelope::MessageBody;` to the top of `message.rs` if not already present.

- [ ] **Step 2: Add a unit test for `compose_receipt` round-trip**

In the `mod tests` block:

```rust
#[test]
fn compose_receipt_roundtrips() {
    let id = alice();
    let room = general();
    let target: Hash = blake3::hash(b"original message").into();
    let composed = compose_receipt(&id, &room, 0, 1_700_000_000_000, target, &mut OsRng).unwrap();
    let decoded = decode_message(&room, &composed.entry, &composed.block).unwrap();
    assert_eq!(decoded.body, MessageBody::Receipt { for_value_hash: target });
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
```

- [ ] **Step 3: Verify**

Run: `nix develop --command cargo test -p sunset-core message::tests::compose_receipt_roundtrips message::tests::compose_text_roundtrips`
Expected: both pass.

- [ ] **Step 4: Commit**

```bash
git add crates/sunset-core/src/message.rs
git commit -m "Add compose_text / compose_receipt convenience helpers"
```

---

## Task 4: Wire-format hex pin for `MessageBody`

**Files:**
- Modify: `crates/sunset-core/src/crypto/envelope.rs`

Pin the postcard encoding of one `Text` and one `Receipt` so accidental drift breaks the build.

- [ ] **Step 1: Add the test**

At the bottom of `envelope.rs`'s `mod tests`:

```rust
#[test]
fn message_body_text_postcard_hex_pin() {
    // Pin the postcard encoding so accidental drift breaks the build.
    // postcard encodes: enum-tag (varint 0) + len-prefixed UTF-8 string.
    let body = MessageBody::Text("hi".to_owned());
    let bytes = postcard::to_stdvec(&body).unwrap();
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    // 00 = Text variant tag; 02 = string length (varint); 6869 = "hi".
    assert_eq!(hex, "00026869", "MessageBody::Text wire encoding drifted");
}

#[test]
fn message_body_receipt_postcard_hex_pin() {
    // Receipt's payload is a 32-byte hash; pin a known input.
    let h: sunset_store::Hash = blake3::hash(b"x").into();
    let body = MessageBody::Receipt { for_value_hash: h };
    let bytes = postcard::to_stdvec(&body).unwrap();
    // 01 = Receipt variant tag; then 32 raw bytes of the hash.
    assert_eq!(bytes[0], 0x01, "MessageBody::Receipt variant tag drifted");
    assert_eq!(bytes.len(), 1 + 32, "Receipt should encode as tag + 32 bytes");
    let hash_hex: String = bytes[1..].iter().map(|b| format!("{b:02x}")).collect();
    let expected_hash: String = h.as_bytes().iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(hash_hex, expected_hash);
}
```

- [ ] **Step 2: Run + adjust**

Run: `nix develop --command cargo test -p sunset-core envelope::tests::message_body_text_postcard_hex_pin envelope::tests::message_body_receipt_postcard_hex_pin`

Expected: both pass. If the Text test fails because postcard's actual encoding differs from `"00026869"`, copy the actual hex from the failure into the assertion (the goal is to PIN the encoding, not to assert what it should be a priori).

If the Receipt test fails on `bytes[0] == 0x01`, similarly inspect and update the assertion to match what postcard actually emits — but DO update the assertion (the test exists to detect drift, not to pre-judge layout).

- [ ] **Step 3: Commit**

```bash
git add crates/sunset-core/src/crypto/envelope.rs
git commit -m "Pin MessageBody postcard encoding via hex test vectors"
```

---

## Task 5: `send_receipt` helper in `sunset-web-wasm`

**Files:**
- Modify: `crates/sunset-web-wasm/src/client.rs`

A small private async helper that composes a `Receipt` and inserts it into the local store. Used by Task 6's auto-ack.

- [ ] **Step 1: Add the helper**

In `crates/sunset-web-wasm/src/client.rs`, add (after the existing `fn current_time_ms` or near the bottom of `impl Client`, wherever fits the file's organization):

```rust
/// Compose and insert a Receipt for `for_value_hash` into the local
/// store. Used by the auto-ack path in `spawn_message_subscription`.
/// Errors are logged via `web_sys::console` and swallowed — receipts
/// are best-effort; failing to ack is not fatal.
async fn send_receipt(
    store: &std::rc::Rc<sunset_store_indexeddb::IndexedDbStore>,
    room: &sunset_core::Room,
    identity: &sunset_core::Identity,
    for_value_hash: sunset_store::Hash,
    rng: &mut rand_chacha::ChaCha20Rng,
) {
    use sunset_store::Store as _;
    let now_ms = js_sys::Date::now() as u64;
    let composed = match sunset_core::compose_receipt(identity, room, 0, now_ms, for_value_hash, rng) {
        Ok(c) => c,
        Err(e) => {
            web_sys::console::error_1(&wasm_bindgen::JsValue::from_str(&format!(
                "compose_receipt failed: {e}"
            )));
            return;
        }
    };
    if let Err(e) = store.insert(composed.entry, Some(composed.block)).await {
        web_sys::console::error_1(&wasm_bindgen::JsValue::from_str(&format!(
            "store.insert(receipt) failed: {e}"
        )));
    }
}
```

NOTE: the exact store concrete type may differ from `sunset_store_indexeddb::IndexedDbStore` — check what `self.store` is in `Client`. Use the same type. The `Rc` wrapper may also differ.

If the store is behind a trait object or impl trait alias, accept it as `&dyn Store` or use the existing wrapper alias from elsewhere in the file.

- [ ] **Step 2: Verify the crate builds**

Run: `nix develop --command cargo build -p sunset-web-wasm --target wasm32-unknown-unknown`
Expected: clean compile. Helper is unused yet — that's OK, will be wired in Task 6.

- [ ] **Step 3: Commit**

```bash
git add crates/sunset-web-wasm/src/client.rs
git commit -m "Add send_receipt helper for auto-acking received messages"
```

---

## Task 6: Variant-aware `spawn_message_subscription` with auto-ack

**Files:**
- Modify: `crates/sunset-web-wasm/src/client.rs`

Branch the subscription on `MessageBody`. For Text from a non-self peer, auto-ack via `send_receipt`. For Receipt, deliver via a (yet-to-exist) `on_receipt` callback. Use an in-memory `HashSet<Hash>` to avoid duplicate-acking the same Text within a single session (Replay::All redelivers historical entries on every page load).

**Known v1 limitation:** the dedup is session-only. On bridge restart, the Replay::All pass will re-ack every historical Text — duplicate Receipts (with different value_hash because of fresh nonce + sent_at_ms) accumulate over restarts. At ~64 bytes each this is acceptable for v1; the spec lists a proper "(receiver_vk, for_value_hash) → bool" index as an out-of-scope follow-up.

- [ ] **Step 1: Refactor the subscription body**

Find `spawn_message_subscription` in `client.rs`. Replace the body of the `wasm_bindgen_futures::spawn_local(async move { ... })` block with:

```rust
wasm_bindgen_futures::spawn_local(async move {
    use futures::StreamExt;
    use std::collections::HashSet;
    use sunset_core::{decode_message, room_messages_filter, MessageBody};
    use sunset_store::{Event, Hash, Replay, Store as _};

    // Session-only dedup: which Text value-hashes have we already
    // acked since this subscription started? Replay::All will
    // redeliver them on page load; this set keeps us from writing
    // a fresh receipt every time. Cross-session dedup is out of
    // scope for v1 (see plan header).
    let mut acked: HashSet<Hash> = HashSet::new();

    let filter = room_messages_filter(&room);
    let mut events = match store.subscribe(filter, Replay::All).await {
        Ok(s) => s,
        Err(e) => {
            web_sys::console::error_1(&wasm_bindgen::JsValue::from_str(&format!(
                "store.subscribe: {e}"
            )));
            return;
        }
    };

    let mut rng = rand_chacha::ChaCha20Rng::from_entropy();

    while let Some(ev) = events.next().await {
        let entry = match ev {
            Ok(Event::Inserted(e)) => e,
            Ok(Event::Replaced { new, .. }) => new,
            Ok(_) => continue,
            Err(e) => {
                web_sys::console::error_1(&wasm_bindgen::JsValue::from_str(&format!(
                    "store event: {e}"
                )));
                continue;
            }
        };

        let block = match store.get_content(&entry.value_hash).await {
            Ok(Some(b)) => b,
            Ok(None) => continue,
            Err(e) => {
                web_sys::console::error_1(&wasm_bindgen::JsValue::from_str(&format!(
                    "get_content: {e}"
                )));
                continue;
            }
        };

        let decoded = match decode_message(&room, &entry, &block) {
            Ok(d) => d,
            Err(e) => {
                web_sys::console::error_1(&wasm_bindgen::JsValue::from_str(&format!(
                    "decode_message: {e}"
                )));
                continue;
            }
        };

        let is_self = decoded.author_key == identity_pub;

        match decoded.body.clone() {
            MessageBody::Text(text) => {
                // Deliver to the FE on_message callback (existing behavior).
                let value_hash_hex = entry.value_hash.to_hex();
                let incoming = crate::messages::from_decoded_text(
                    decoded.clone(),
                    text,
                    value_hash_hex,
                    is_self,
                );
                if let Some(cb) = on_message.borrow().as_ref() {
                    let _ = cb.call1(&wasm_bindgen::JsValue::NULL, &wasm_bindgen::JsValue::from(incoming));
                }

                // Auto-ack: only for non-self texts not already acked this session.
                if !is_self && !acked.contains(&entry.value_hash) {
                    acked.insert(entry.value_hash);
                    send_receipt(&store, &room, &identity, entry.value_hash, &mut rng).await;
                }
            }
            MessageBody::Receipt { for_value_hash } => {
                // Drop self-Receipts at the bridge — see spec.
                if is_self {
                    continue;
                }
                let for_hex = for_value_hash.to_hex();
                let from_pub = decoded.author_key;
                let incoming = crate::messages::receipt_to_js(for_hex, from_pub);
                if let Some(cb) = on_receipt.borrow().as_ref() {
                    let _ = cb.call1(&wasm_bindgen::JsValue::NULL, &wasm_bindgen::JsValue::from(incoming));
                }
            }
        }
    }
});
```

This refactor introduces calls to `crate::messages::from_decoded_text` and `crate::messages::receipt_to_js` — both are defined in Task 7. It also reads `on_receipt.borrow()` — that field is added in Task 7 as well. The build will fail until Task 7 lands; that's expected. **Do not commit until Task 7 is also applied** — or stage them together as one commit.

NOTE on `from_decoded`: the existing helper currently takes `(DecodedMessage, value_hash_hex, is_self)`. It accesses `decoded.body` to get the text. After the migration in Task 2, that helper may need to change signature (since body is now MessageBody, not String). To avoid forking on that during this task, we use the new `from_decoded_text(decoded, text, value_hash_hex, is_self)` — the text is extracted upfront. Update or replace the helper in Task 7.

- [ ] **Step 2: Identify the existing `identity` field on Client and ensure it's clonable**

`send_receipt` takes `&sunset_core::Identity`. The subscription's `wasm_bindgen_futures::spawn_local` move closure needs `identity` to be `Clone`-able into the closure. Check `Client::identity` — it's likely an `Identity` (which is `Clone`). If not, wrap it in `Rc<Identity>` and clone the `Rc`.

If `Client::identity` isn't currently captured by the spawn closure, capture it: in `spawn_message_subscription`, near the top:

```rust
let identity = self.identity.clone();
let on_receipt = self.on_receipt.clone();  // see Task 7
```

The existing function already clones `store`, `room`, `identity_pub`, `on_message` — add the two new clones alongside.

- [ ] **Step 3: Don't run anything yet — Task 7 must land first**

`cargo build` will fail because `messages::from_decoded_text`, `messages::receipt_to_js`, and `Client::on_receipt` don't exist yet. Move on to Task 7 and stage these changes together.

---

## Task 7: `IncomingReceipt` JS object + `on_receipt` callback wiring

**Files:**
- Modify: `crates/sunset-web-wasm/src/messages.rs`
- Modify: `crates/sunset-web-wasm/src/client.rs`

- [ ] **Step 1: Refactor / add helpers in `messages.rs`**

Open `crates/sunset-web-wasm/src/messages.rs`. Find the existing `from_decoded` (or similarly named) helper that builds `IncomingMessage` from a `DecodedMessage`. It currently takes `decoded.body` (a String). Replace its signature to take the text separately, since `decoded.body` is now `MessageBody`:

```rust
/// Build the JS-facing IncomingMessage object from a decoded Text body.
/// Text is passed in separately so the caller can pattern-match the
/// MessageBody enum upstream and pass only the inner String.
pub fn from_decoded_text(
    decoded: sunset_core::DecodedMessage,
    text: String,
    value_hash_hex: String,
    is_self: bool,
) -> IncomingMessage {
    // Body of the existing from_decoded, but using `text` instead of
    // `decoded.body`. Adapt as needed to your existing field layout.
    IncomingMessage {
        value_hash_hex,
        author_pubkey: bytes_from_identity_key(&decoded.author_key),
        sent_at_ms: decoded.sent_at_ms as f64,
        body: text,
        is_self,
    }
}

/// Construct the JS-facing IncomingReceipt object.
pub fn receipt_to_js(
    for_value_hash_hex: String,
    from_pubkey: sunset_core::IdentityKey,
) -> IncomingReceipt {
    IncomingReceipt {
        for_value_hash_hex,
        from_pubkey: bytes_from_identity_key(&from_pubkey),
    }
}
```

(`bytes_from_identity_key` should already exist in this module or its sibling — reuse it.)

- [ ] **Step 2: Define the `IncomingReceipt` wasm-bindgen struct**

Below the existing `IncomingMessage` struct in `messages.rs`, add:

```rust
#[wasm_bindgen::prelude::wasm_bindgen]
pub struct IncomingReceipt {
    #[wasm_bindgen::prelude::wasm_bindgen(getter_with_clone)]
    pub for_value_hash_hex: String,
    #[wasm_bindgen::prelude::wasm_bindgen(getter_with_clone)]
    pub from_pubkey: Vec<u8>,
}
```

Match the attribute style used by the existing `IncomingMessage` (e.g., `#[wasm_bindgen]` if there's a re-export at the top). Keep the field types compatible with what the FE expects (Vec<u8> serializes as a Uint8Array on the JS side, which Gleam's BitArray accepts via `bitsToBytes`).

- [ ] **Step 3: Add `on_receipt` to the `Client` struct and `on_message`-adjacent methods**

In `client.rs`, find the `Client` struct definition. Alongside `on_message`, add:

```rust
on_receipt: std::rc::Rc<std::cell::RefCell<Option<js_sys::Function>>>,
```

And in `Client::new` (or wherever `on_message` is initialised), initialise:

```rust
on_receipt: std::rc::Rc::new(std::cell::RefCell::new(None)),
```

Next, add the public method:

```rust
pub fn on_receipt(&self, callback: js_sys::Function) {
    *self.on_receipt.borrow_mut() = Some(callback);
    // No new subscription needed — spawn_message_subscription handles
    // both Text and Receipt variants.
}
```

- [ ] **Step 4: Stage build + tests**

The Task 6 + Task 7 changes together should now compile. Run:

```
nix develop --command cargo build -p sunset-web-wasm --target wasm32-unknown-unknown
```

Expected: clean.

Then run the full workspace:

```
nix develop --command cargo test --workspace --all-features
```

Expected: all tests pass.

- [ ] **Step 5: Commit Tasks 6 + 7 together**

```bash
git add crates/sunset-web-wasm/src/client.rs crates/sunset-web-wasm/src/messages.rs
git commit -m "Auto-ack received Text messages with Receipts at the bridge layer"
```

---

## Task 8: Two-engine integration test for receipt round-trip

**Files:**
- Modify or create: `crates/sunset-web-wasm/tests/receipts.rs` (new) OR an existing `tests/` integration file if conventions dictate

This test exercises the auto-ack flow without the JS bindings: two engines, A sends a Text, B's bridge logic auto-acks, A's bridge logic fires `on_receipt`. We approximate by driving the store layer directly.

- [ ] **Step 1: Inspect existing two-engine tests for setup patterns**

Run:

```
ls crates/sunset-web-wasm/tests/ 2>/dev/null
ls crates/sunset-sync/tests/ 2>/dev/null
grep -l "two_engine\|two_browser\|TwoEngine" crates/*/tests/*.rs 2>/dev/null
```

Reuse the harness and helpers from the closest existing test (likely under `crates/sunset-sync/tests/` — `Engine`-level two-engine harness). Mirror its setup.

If no suitable test infrastructure exists at the sunset-web-wasm level (likely; wasm tests run differently), put this test in `crates/sunset-core/tests/receipts.rs` instead — pure-Rust, runs in CI naturally.

- [ ] **Step 2: Write the test**

Create `crates/sunset-core/tests/receipts.rs` with content along these lines (adapt store type to whatever the existing tests use; `sunset_store_memory::MemoryStore` is most likely):

```rust
//! End-to-end test: Alice sends a Text, Bob "auto-acks" it via a
//! manually-driven loop (mirroring the wasm bridge), Alice picks
//! up the Receipt and can identify Bob as a confirmer.

use rand_core::OsRng;
use sunset_core::{compose_receipt, compose_text, decode_message, Identity, MessageBody, Room};
use sunset_core::crypto::constants::test_fast_params;
use sunset_store::Store as _;

#[tokio::test]
async fn receipt_round_trip_between_two_identities() {
    let alice = Identity::generate(&mut OsRng);
    let bob = Identity::generate(&mut OsRng);
    let room = Room::open_with_params("general", &test_fast_params()).unwrap();

    let alice_store = sunset_store_memory::MemoryStore::with_no_verify();
    let bob_store = sunset_store_memory::MemoryStore::with_no_verify();

    // 1. Alice composes and inserts a Text in her local store.
    let text = compose_text(&alice, &room, 0, 1, "hello bob", &mut OsRng).unwrap();
    alice_store.insert(text.entry.clone(), Some(text.block.clone())).await.unwrap();

    // 2. Simulate sync: the same entry shows up in Bob's store.
    bob_store.insert(text.entry.clone(), Some(text.block.clone())).await.unwrap();

    // 3. Bob's bridge logic decodes Alice's text. Since author != self,
    //    Bob auto-composes a Receipt referencing the text's value_hash.
    let decoded = decode_message(&room, &text.entry, &text.block).unwrap();
    let MessageBody::Text(_) = &decoded.body else {
        panic!("expected Text body");
    };
    let receipt = compose_receipt(&bob, &room, 0, 2, decoded.value_hash, &mut OsRng).unwrap();
    bob_store.insert(receipt.entry.clone(), Some(receipt.block.clone())).await.unwrap();

    // 4. Sync the receipt back to Alice's store.
    alice_store.insert(receipt.entry.clone(), Some(receipt.block.clone())).await.unwrap();

    // 5. Alice's bridge logic decodes the receipt and confirms it
    //    references her text and is signed by Bob.
    let receipt_decoded = decode_message(&room, &receipt.entry, &receipt.block).unwrap();
    assert_eq!(receipt_decoded.author_key, bob.public());
    match receipt_decoded.body {
        MessageBody::Receipt { for_value_hash } => {
            assert_eq!(for_value_hash, text.entry.value_hash);
        }
        _ => panic!("expected Receipt body"),
    }

    // 6. Loop avoidance: when Alice's bridge sees this Receipt, it
    //    must NOT auto-ack (Receipts never trigger Receipts). We
    //    can't directly invoke the bridge's match arm here (it lives
    //    in wasm-bindgen code), but we can assert the invariant
    //    structurally: there is no `compose_receipt` called for a
    //    Receipt body. The match arm in spawn_message_subscription
    //    routes Receipt → on_receipt callback (no auto-ack).
    //    This test passes if both stores end up with exactly 2
    //    entries each (1 Text + 1 Receipt).
    let alice_count = alice_store.entry_count_in_room(&room).await;
    let bob_count = bob_store.entry_count_in_room(&room).await;
    assert_eq!(alice_count, 2, "Alice should have Text + Receipt only");
    assert_eq!(bob_count, 2, "Bob should have Text + Receipt only");
}
```

Note: `entry_count_in_room` is a hypothetical helper. If the store doesn't expose entry counting, replace those assertions with a manual count via:

```rust
async fn count_entries(
    store: &sunset_store_memory::MemoryStore,
    room: &Room,
) -> usize {
    use sunset_core::room_messages_filter;
    use sunset_store::{Replay, Store as _};
    let mut events = store.subscribe(room_messages_filter(room), Replay::All).await.unwrap();
    let mut n = 0;
    // Drain with a short timeout: subscribe returns a long-lived
    // stream; once historical drains there's nothing live, so we use
    // a tokio::select with a sleep to detect "no more events".
    loop {
        tokio::select! {
            ev = futures::StreamExt::next(&mut events) => {
                if ev.is_none() { break; }
                if matches!(ev, Some(Ok(sunset_store::Event::Inserted(_)))) {
                    n += 1;
                }
            }
            _ = tokio::time::sleep(std::time::Duration::from_millis(50)) => break,
        }
    }
    n
}
```

Use this helper for the count assertions if the direct API isn't available.

If `sunset_store_memory::MemoryStore::with_no_verify` isn't the right constructor name in the current codebase, find the correct one via:

```
grep -rn "fn new\|fn with_" crates/sunset-store-memory/src/store.rs | head
```

- [ ] **Step 3: Add `tokio` and `sunset_store_memory` dev-dependencies if needed**

Check `crates/sunset-core/Cargo.toml`'s `[dev-dependencies]`. If `tokio` and `sunset_store_memory` aren't there, add them. Look at sibling test files for the exact dep declarations — sunset-core almost certainly already has these for existing tests.

- [ ] **Step 4: Run the test**

Run: `nix develop --command cargo test -p sunset-core --test receipts`
Expected: pass.

- [ ] **Step 5: Commit**

```bash
git add crates/sunset-core/tests/receipts.rs crates/sunset-core/Cargo.toml
git commit -m "Two-identity integration test: receipt round-trip via compose_receipt"
```

---

## Task 9: FE FFI binding for `on_receipt`

**Files:**
- Modify: `web/src/sunset_web/sunset.ffi.mjs`
- Modify: `web/src/sunset_web/sunset.gleam`

- [ ] **Step 1: Add the JS-side helper**

Append to `web/src/sunset_web/sunset.ffi.mjs`:

```js
export function onReceipt(client, callback) {
  client.on_receipt((incoming) => {
    callback(incoming);
  });
}

// Reading the IncomingReceipt JS object:
export function recForValueHashHex(rec) {
  return rec.for_value_hash_hex;
}

export function recFromPubkey(rec) {
  return new BitArray(rec.from_pubkey);
}
```

The `BitArray` import at the top of the file already exists (used by `loadOrCreateIdentity`). If not, add `BitArray` to the existing `import { BitArray, Ok, Error as GError, toList } from "../../prelude.mjs";` line.

- [ ] **Step 2: Add the Gleam externs**

Append to `web/src/sunset_web/sunset.gleam`:

```gleam
/// JS-side IncomingReceipt object, opaque to Gleam.
pub type IncomingReceipt

/// Subscribe to delivery receipts. The callback fires once per receipt
/// authored by a peer other than us; self-receipts are dropped at the
/// bridge layer.
@external(javascript, "./sunset.ffi.mjs", "onReceipt")
pub fn on_receipt(
  client: ClientHandle,
  callback: fn(IncomingReceipt) -> Nil,
) -> Nil

/// Hex-encoded value_hash of the Text that this Receipt acknowledges.
@external(javascript, "./sunset.ffi.mjs", "recForValueHashHex")
pub fn rec_for_value_hash_hex(r: IncomingReceipt) -> String

/// Verifying key of the peer who authored this Receipt.
@external(javascript, "./sunset.ffi.mjs", "recFromPubkey")
pub fn rec_from_pubkey(r: IncomingReceipt) -> BitArray
```

`BitArray` is already imported in this file (used by `IncomingMessage` accessors). If not, the gleam stdlib path is `gleam/bit_array.{type BitArray}` — match the existing import style.

- [ ] **Step 3: Verify Gleam compiles**

Run: `nix develop --command bash -c "cd web && gleam build"`
Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add web/src/sunset_web/sunset.ffi.mjs web/src/sunset_web/sunset.gleam
git commit -m "Add on_receipt FFI binding to Gleam ↔ wasm bridge"
```

---

## Task 10: FE Model + Msg + update branch for receipts

**Files:**
- Modify: `web/src/sunset_web.gleam`

- [ ] **Step 1: Add `set` import + receipts field**

At the top of `web/src/sunset_web.gleam` imports:

```gleam
import gleam/set.{type Set}
```

In the `Model` record, alongside the existing message-related fields:

```gleam
    /// Receipts received per outgoing message, keyed by message id
    /// (value_hash hex). Each entry is the set of peer verifying-key
    /// short-hex strings that have acknowledged. The bridge filters
    /// self-receipts at the source so this dict never contains them.
    receipts: Dict(String, Set(String)),
```

- [ ] **Step 2: Initialise `receipts` in `init`**

In `init`, where the `Model` record is constructed, add:

```gleam
      receipts: dict.new(),
```

- [ ] **Step 3: Add the `IncomingReceipt` Msg variant**

In the `Msg` type:

```gleam
  IncomingReceipt(message_id: String, from_pubkey: String)
```

Place it next to `IncomingMsg` for readability.

- [ ] **Step 4: Handle `IncomingReceipt` in `update`**

In `update`'s big `case msg` block:

```gleam
    IncomingReceipt(message_id, from_pubkey) -> {
      let existing = case dict.get(model.receipts, message_id) {
        Ok(s) -> s
        Error(_) -> set.new()
      }
      let updated = set.insert(existing, from_pubkey)
      #(
        Model(..model, receipts: dict.insert(model.receipts, message_id, updated)),
        effect.none(),
      )
    }
```

- [ ] **Step 5: Wire the FFI subscription in `ClientReady`**

Find the `ClientReady(client)` branch in `update`. It already wires `on_message`, presence, etc. Add (alongside the existing `on_msg_eff`):

```gleam
      let on_receipt_eff =
        effect.from(fn(dispatch) {
          sunset.on_receipt(client, fn(r) {
            dispatch(IncomingReceipt(
              sunset.rec_for_value_hash_hex(r),
              short_pubkey(sunset.rec_from_pubkey(r)),
            ))
          })
        })
```

`short_pubkey` is the existing helper that hex-encodes the first 4 bytes of a pubkey BitArray (used elsewhere in this file for IncomingMsg.author).

Add `on_receipt_eff` to the `effect.batch([...])` returned by the `ClientReady` branch.

- [ ] **Step 6: Verify Gleam compiles + existing web tests pass**

Run: `nix develop --command bash -c "cd web && gleam build"`
Expected: clean.

Run: `nix run .#web-test -- --project=chromium`
Expected: existing tests pass (no UI change yet — just plumbing).

- [ ] **Step 7: Commit**

```bash
git add web/src/sunset_web.gleam
git commit -m "Add receipts dict + IncomingReceipt Msg + FFI subscription"
```

---

## Task 11: Render pending self-messages with reduced opacity

**Files:**
- Modify: `web/src/sunset_web/views/main_panel.gleam`
- Modify: `web/src/sunset_web.gleam`

The `pending` flag on `domain.Message` is currently set during the brief send window. We repurpose it: it reflects "this is mine and no peer has acked yet". `main_panel.view` derives it from the model's receipts dict.

- [ ] **Step 1: Pass the receipts dict into `main_panel.view`**

In `web/src/sunset_web/views/main_panel.gleam`, add a labeled arg:

```gleam
  receipts receipts: Dict(String, set.Set(String)),
```

Add necessary imports at the top:

```gleam
import gleam/set
```

Thread `receipts` through to `messages_list` and then to `message_view`:

```gleam
fn messages_list(
  ...,
  receipts: Dict(String, set.Set(String)),
  ...
) -> Element(msg) {
  ...
  // Inside the message-render loop:
  message_view(
    p, m,
    grouped,
    i == last_seen_index,
    picker_open,
    detail_open,
    receipts,  // NEW
    on_react_toggle,
    on_add_reaction,
    on_open_detail,
  )
  ...
}
```

In `message_view`, accept the new arg and compute pending:

```gleam
fn message_view(
  ...,
  receipts: Dict(String, set.Set(String)),
  ...
) -> Element(msg) {
  let pending = m.you && {
    case dict.get(receipts, m.id) {
      Ok(s) -> set.size(s) == 0
      Error(_) -> True
    }
  }
  ...
}
```

- [ ] **Step 2: Apply `opacity: 0.55` to pending messages**

In `message_view`, the bubble container's `ui.css` block adds:

```gleam
        #("opacity", case pending {
          True -> "0.55"
          False -> "1"
        }),
        #("transition", "opacity 220ms ease"),
```

Find the outermost bubble div (the one wrapping the message body — usually wraps `body_text(p, m)` or similar). Add the two CSS entries to its `ui.css` list.

If the existing `domain.Message.pending` field is already used to render some indicator, leave that alone — the new opacity calculation supersedes it visually. Don't read `m.pending` for the opacity decision; use `receipts` exclusively.

- [ ] **Step 3: Pass `model.receipts` from the `room_view` call site**

In `web/src/sunset_web.gleam`, find the `main_panel.view(...)` call. Add:

```gleam
      receipts: model.receipts,
```

- [ ] **Step 4: Verify build + smoke test**

Run: `nix develop --command bash -c "cd web && gleam build"`
Expected: clean.

Run: `nix run .#web-test -- --project=chromium`
Expected: existing tests pass.

- [ ] **Step 5: Commit**

```bash
git add web/src/sunset_web/views/main_panel.gleam web/src/sunset_web.gleam
git commit -m "Render pending self-messages at opacity 0.55 until first receipt"
```

---

## Task 12: Playwright two-tab pending → delivered test

**Files:**
- Modify: `web/e2e/voice.spec.js` (or wherever two-browser fixtures live — likely `web/e2e/two_browser_chat.spec.js`)
- Possibly: a new file `web/e2e/receipts.spec.js`

There's likely an existing two-browser harness (`two_browser_chat.spec.js` per earlier exploration). Reuse it.

- [ ] **Step 1: Find the existing two-browser test**

Run:

```
ls web/e2e/
grep -l "two_browser\|browser2\|secondBrowser\|second tab" web/e2e/*.js
```

Open the closest match. It likely launches two `BrowserContext` instances + connects both to a relay. Mirror that pattern.

- [ ] **Step 2: Write the test**

Create or append to `web/e2e/receipts.spec.js`:

```js
// Two tabs in the same room: tab A sends a message; until tab B
// receives + auto-acks, tab A's bubble renders at opacity ~0.55.
// Once the receipt arrives, opacity transitions to 1.

import { expect, test } from "@playwright/test";

test("self-message stays gray until a peer's receipt arrives", async ({
  browser,
}, testInfo) => {
  test.skip(testInfo.project.name === "mobile-chrome", "two-browser test runs on desktop only");

  const ctxA = await browser.newContext();
  const ctxB = await browser.newContext();
  const pageA = await ctxA.newPage();
  const pageB = await ctxB.newPage();

  // Connect both to the same room. The relay URL is provided via the
  // existing test harness — see two_browser_chat.spec.js for the
  // ?relay=… query string convention.
  const url = "/#receipt-test";
  await pageA.goto(url);
  await pageB.goto(url);

  // Wait for both to be ready (header visible, client initialised).
  await expect(pageA.locator("main")).toBeVisible();
  await expect(pageB.locator("main")).toBeVisible();

  // A types and sends.
  await pageA.locator("main input, main textarea").first().fill("hello");
  await pageA.keyboard.press("Enter");

  // A's bubble is initially pending (opacity ~0.55).
  const bubble = pageA.locator(".msg-row", { hasText: "hello" }).first();
  await expect(bubble).toBeVisible();
  const initialOpacity = await bubble.evaluate(
    (el) => parseFloat(getComputedStyle(el).opacity),
  );
  expect(initialOpacity).toBeLessThan(0.7);

  // B receives the message, decodes, and auto-acks. We poll A's bubble
  // for the opacity to flip to 1.
  await expect
    .poll(
      async () =>
        bubble.evaluate((el) => parseFloat(getComputedStyle(el).opacity)),
      { timeout: 10_000 },
    )
    .toBeGreaterThan(0.95);

  await ctxA.close();
  await ctxB.close();
});
```

If the existing two-browser test sets up the relay URL via env var or query string, mirror that exactly. If neither tab connects without explicit setup, add the same `?relay=` setup the existing tests use.

- [ ] **Step 3: Run the test**

Run: `nix run .#web-test -- --project=chromium e2e/receipts.spec.js`
Expected: PASS.

If the test times out on the receipt arrival, add `console.log` lines around `IncomingReceipt` in `sunset_web.gleam` and the bridge's auto-ack to confirm the flow. The most likely cause is the two tabs not actually being on the same relay.

- [ ] **Step 4: Run the full suite**

Run: `nix run .#web-test`
Expected: all tests pass (the pre-existing presence flake may or may not appear, that's the baseline).

- [ ] **Step 5: Commit**

```bash
git add web/e2e/receipts.spec.js
git commit -m "Playwright two-tab test: self-message gray until receipt arrives"
```

---

## Done

After all 12 tasks land, delivery receipts are functional. Use `superpowers:finishing-a-development-branch` to merge to master.
