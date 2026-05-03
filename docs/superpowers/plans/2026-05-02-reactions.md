# Reactions Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Wire emoji reactions through the existing chat message path — `MessageBody::Reaction { for_value_hash, emoji, action }` variant in sunset-core, a self-driven `ReactionTracker` in sunset-core, mechanical bridge wiring in sunset-web-wasm, and replacement of the FE's fixture-driven reaction state with bridge-driven snapshots while keeping the existing chip UI.

**Architecture:** Reactions ride the same `<room_fp>/msg/<value_hash>` namespace as text + receipts — same AEAD envelope, same Ed25519 inner signature. Each tap writes one signed entry; the `ReactionTracker` (parallel to `MembershipTracker`) subscribes to the room's message namespace, decodes `Reaction` variants, applies LWW per `(author, target, emoji)` keyed on `(sent_at_ms, value_hash)`, and fires whole-snapshot callbacks on debounced state changes. The FE swaps its local `toggle_reaction` fold for a bridge-driven `Dict(target_hex, Dict(emoji, Set(author_hex)))` model.

**Tech Stack:** Rust (sunset-core, sunset-web-wasm), postcard wire format, Gleam/Lustre (web/), `emoji-picker-element` web component for the full picker.

**Spec:** `docs/superpowers/specs/2026-05-02-reactions-design.md`

---

## File Map

### sunset-core (wire format + tracker)

- **Modify** `crates/sunset-core/src/crypto/envelope.rs` — add `Reaction` variant to `MessageBody`, add `ReactionAction` enum, add hex-pinned wire-format tests.
- **Modify** `crates/sunset-core/src/error.rs` — add `EmojiTooLong { len: usize }` variant.
- **Modify** `crates/sunset-core/src/message.rs` — add `compose_reaction` helper; add decode-side defensive emoji length check.
- **Create** `crates/sunset-core/src/reactions.rs` — `ReactionEvent`, `ReactionSnapshot`, `ReactionSig`, `ReactionHandles`, pure helpers (`apply_event`, `derive_snapshot`, `reactions_signature`), and `spawn_reaction_tracker`.
- **Modify** `crates/sunset-core/src/lib.rs` — `pub mod reactions;` + re-exports.
- **Create** `crates/sunset-core/tests/reactions_tracker.rs` — end-to-end test against `MemoryStore`.

### sunset-web-wasm (bridge)

- **Modify** `crates/sunset-web-wasm/src/client.rs` — add `reaction_handles` field; spawn the tracker in `Client::new`; add `Client::on_reactions_changed` and `Client::send_reaction`.
- **Create** `crates/sunset-web-wasm/src/reactions.rs` — JS marshaling helpers (snapshot → JS Map<emoji, Set<author_hex>>; payload object).
- **Modify** `crates/sunset-web-wasm/src/lib.rs` — `mod reactions;`.

### web/ (FE wiring)

- **Modify** `web/src/sunset_web/sunset.gleam` — FFI bindings for `on_reactions_changed`, `send_reaction`, plus a fresh JS-side type `IncomingReactionsSnapshot`.
- **Modify** `web/src/sunset_web/sunset.ffi.mjs` — JS implementations of those FFI functions.
- **Modify** `web/src/sunset_web.gleam` — replace `Model.reactions` shape (raw snapshot dict), replace `AddReaction` update branch (call bridge), drop `seed_reactions` + `toggle_reaction`, add `ReactionsChanged` Msg, view-time conversion of snapshot to `List(Reaction)`, init wiring.
- **Modify** `web/src/sunset_web/views/main_panel.gleam` — add a "+" button at end of quick row that opens the full picker; render the picker.
- **Create** `web/src/sunset_web/views/emoji_picker.gleam` — wraps the `emoji-picker-element` web component as a Lustre element.
- **Modify** `web/package.json` — add `emoji-picker-element` dependency.
- **Modify** `flake.nix` — wire the npm dep through hermetic build.
- **Modify** `web/test/playwright/` (existing) — add reactions E2E test alongside existing receipts tests.

---

## Phase A — sunset-core: wire format + compose helper

### Task A1: Add `ReactionAction` enum and `Reaction` variant to `MessageBody`

**Files:**
- Modify: `crates/sunset-core/src/crypto/envelope.rs`

- [ ] **Step 1: Add the failing wire-format pin tests**

Append to the `#[cfg(test)] mod tests` block in `crates/sunset-core/src/crypto/envelope.rs`:

```rust
#[test]
fn message_body_reaction_add_postcard_hex_pin() {
    let h: sunset_store::Hash = blake3::hash(b"x").into();
    let body = MessageBody::Reaction {
        for_value_hash: h,
        emoji: "👍".to_owned(),
        action: ReactionAction::Add,
    };
    let bytes = postcard::to_stdvec(&body).unwrap();
    // 02 = Reaction variant tag (third variant after Text=00, Receipt=01).
    assert_eq!(bytes[0], 0x02, "MessageBody::Reaction variant tag drifted");
    // Then 32 hash bytes; then varint emoji-len (4 for 👍 = F0 9F 91 8D);
    // then 4 emoji bytes; then enum-tag for ReactionAction (00 for Add).
    assert_eq!(
        bytes.len(),
        1 + 32 + 1 + 4 + 1,
        "Reaction Add encoding drifted: tag + hash + len + emoji + action"
    );
    assert_eq!(bytes[1 + 32], 0x04, "emoji length varint drifted");
    assert_eq!(&bytes[1 + 32 + 1..1 + 32 + 1 + 4], "👍".as_bytes());
    assert_eq!(bytes[1 + 32 + 1 + 4], 0x00, "ReactionAction::Add tag drifted");
}

#[test]
fn message_body_reaction_remove_postcard_hex_pin() {
    let h: sunset_store::Hash = blake3::hash(b"x").into();
    let body = MessageBody::Reaction {
        for_value_hash: h,
        emoji: "❤".to_owned(), // 3-byte emoji, no VS-16
        action: ReactionAction::Remove,
    };
    let bytes = postcard::to_stdvec(&body).unwrap();
    assert_eq!(bytes[0], 0x02);
    assert_eq!(*bytes.last().unwrap(), 0x01, "ReactionAction::Remove tag drifted");
}

#[test]
fn message_body_reaction_roundtrips_via_postcard() {
    let h: sunset_store::Hash = blake3::hash(b"target").into();
    let body = MessageBody::Reaction {
        for_value_hash: h,
        emoji: "🎉".to_owned(),
        action: ReactionAction::Add,
    };
    let bytes = postcard::to_stdvec(&body).unwrap();
    let decoded: MessageBody = postcard::from_bytes(&bytes).unwrap();
    assert_eq!(decoded, body);
}

#[test]
fn message_body_text_postcard_hex_pin_unchanged() {
    // Confirms that adding the Reaction variant did not reorder existing
    // tag values: Text must still encode to 00026869.
    let body = MessageBody::Text("hi".to_owned());
    let bytes = postcard::to_stdvec(&body).unwrap();
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(hex, "00026869");
}
```

- [ ] **Step 2: Run tests and verify they fail**

```
nix develop --command cargo test -p sunset-core message_body_reaction
```

Expected: compile errors — `MessageBody::Reaction` and `ReactionAction` undefined.

- [ ] **Step 3: Add the variant + enum**

In `crates/sunset-core/src/crypto/envelope.rs`, replace the existing `MessageBody` definition (and add `ReactionAction` directly above it):

```rust
/// Add or Remove for a `MessageBody::Reaction` event. The application
/// layer folds a stream of these per `(author, target, emoji)` to derive
/// "is this author currently reacting with this emoji on this target?".
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReactionAction {
    Add,
    Remove,
}

/// Discriminator for the inner plaintext of a chat-room entry. All
/// variants ride the same `<room_fp>/msg/<value_hash>` namespace and
/// share the AEAD envelope; only the plaintext shape differs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MessageBody {
    /// A user-authored chat message.
    Text(String),
    /// An acknowledgement that the author of this entry decoded the
    /// referenced `Text` message. The author of the receipt is the
    /// receiver of the original message.
    Receipt {
        for_value_hash: sunset_store::Hash,
    },
    /// An emoji reaction attached to the referenced message. The
    /// author of the entry is the reactor; `for_value_hash` is the
    /// `value_hash` of the message being reacted to. Per
    /// `(author, for_value_hash, emoji)`, the application folds events
    /// LWW by `(sent_at_ms, value_hash)` to derive current state.
    Reaction {
        for_value_hash: sunset_store::Hash,
        emoji: String,
        action: ReactionAction,
    },
}
```

- [ ] **Step 4: Run tests to verify they pass**

```
nix develop --command cargo test -p sunset-core message_body_reaction
```

Expected: 3 new tests pass; existing `message_body_text_postcard_hex_pin` still passes (variant tag for `Text` unchanged).

- [ ] **Step 5: Run full sunset-core suite to verify no regressions**

```
nix develop --command cargo test -p sunset-core
```

Expected: all pass. The new `Reaction` variant has compiler-enforced exhaustive matches; if anything else in the crate matches on `MessageBody`, it breaks here.

- [ ] **Step 6: Commit**

```bash
git add crates/sunset-core/src/crypto/envelope.rs
git commit -m "$(cat <<'EOF'
sunset-core: add MessageBody::Reaction variant + ReactionAction enum

Variants are added (not reordered), so existing Text/Receipt entries
decode unchanged. Hex-pinned wire-format tests for Reaction Add/Remove
detect future drift.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task A2: Add `EmojiTooLong` error variant

**Files:**
- Modify: `crates/sunset-core/src/error.rs`

- [ ] **Step 1: Add the variant**

In `crates/sunset-core/src/error.rs`, add to the `Error` enum (alongside `PayloadTooLarge`):

```rust
#[error("emoji exceeds 64-byte limit: {len} bytes")]
EmojiTooLong { len: usize },
```

- [ ] **Step 2: Verify the crate still builds**

```
nix develop --command cargo build -p sunset-core
```

Expected: clean build. No tests yet — the variant is unused until Task A3 wires it in.

- [ ] **Step 3: Commit**

```bash
git add crates/sunset-core/src/error.rs
git commit -m "$(cat <<'EOF'
sunset-core: add EmojiTooLong error variant

Used by compose_reaction (Task A3) and decode_message (Task A4) to
reject emoji strings beyond the 64-byte cap.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task A3: Add `compose_reaction` helper

**Files:**
- Modify: `crates/sunset-core/src/message.rs`

- [ ] **Step 1: Write the failing tests**

Append to the `#[cfg(test)] mod tests` block in `crates/sunset-core/src/message.rs`:

```rust
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
        target,
        "👍",
        crate::crypto::envelope::ReactionAction::Add,
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
        target,
        "🎉",
        crate::crypto::envelope::ReactionAction::Remove,
        &mut OsRng,
    )
    .unwrap();
    let decoded = decode_message(&room, &composed.entry, &composed.block).unwrap();
    assert!(matches!(
        decoded.body,
        MessageBody::Reaction { action: crate::crypto::envelope::ReactionAction::Remove, .. }
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
        target,
        &oversized,
        crate::crypto::envelope::ReactionAction::Add,
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
        target,
        &max_size,
        crate::crypto::envelope::ReactionAction::Add,
        &mut OsRng,
    );
    assert!(result.is_ok(), "64 bytes should be accepted (limit is inclusive)");
}
```

- [ ] **Step 2: Run tests to verify they fail**

```
nix develop --command cargo test -p sunset-core compose_reaction
```

Expected: compile errors — `compose_reaction` undefined.

- [ ] **Step 3: Add the helper**

In `crates/sunset-core/src/message.rs`, update the imports at the top:

```rust
use crate::crypto::envelope::{
    EncryptedMessage, MessageBody, ReactionAction, SignedMessage, inner_sig_payload_bytes,
};
```

Then add this function below `compose_receipt`:

```rust
/// Compose a reaction event. `for_value_hash` is the `value_hash` of
/// the message being reacted to; `emoji` is a free-form unicode string
/// (caller-validated; we only enforce the 64-byte length cap which
/// covers all unicode emoji including ZWJ family sequences).
pub fn compose_reaction<R: CryptoRngCore + ?Sized>(
    identity: &Identity,
    room: &Room,
    epoch_id: u64,
    sent_at_ms: u64,
    for_value_hash: Hash,
    emoji: &str,
    action: ReactionAction,
    rng: &mut R,
) -> Result<ComposedMessage> {
    if emoji.len() > 64 {
        return Err(Error::EmojiTooLong { len: emoji.len() });
    }
    compose_message(
        identity,
        room,
        epoch_id,
        sent_at_ms,
        MessageBody::Reaction {
            for_value_hash,
            emoji: emoji.to_owned(),
            action,
        },
        rng,
    )
}
```

- [ ] **Step 4: Run tests to verify they pass**

```
nix develop --command cargo test -p sunset-core compose_reaction
```

Expected: 4 tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/sunset-core/src/message.rs
git commit -m "$(cat <<'EOF'
sunset-core: add compose_reaction helper

Parallel to compose_text/compose_receipt. Validates the 64-byte emoji
cap and returns Error::EmojiTooLong on overflow; otherwise delegates
to compose_message with MessageBody::Reaction.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task A4: Defensive decode-side emoji length check

**Files:**
- Modify: `crates/sunset-core/src/message.rs`

- [ ] **Step 1: Write the failing test**

Append to the `#[cfg(test)] mod tests` block in `crates/sunset-core/src/message.rs`:

```rust
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
```

- [ ] **Step 2: Run test to verify it fails**

```
nix develop --command cargo test -p sunset-core decode_rejects_oversized_reaction_emoji
```

Expected: FAIL — currently decode does not enforce the cap, so the test panics on `unwrap_err`.

- [ ] **Step 3: Add the defensive check in `decode_message`**

In `crates/sunset-core/src/message.rs`, immediately after the inner-signature verification (`author_key.verify(&inner_payload, &dalek_sig)?;` line) and before the `Ok(DecodedMessage { ... })` return, insert:

```rust
    if let MessageBody::Reaction { ref emoji, .. } = signed.body {
        if emoji.len() > 64 {
            return Err(Error::EmojiTooLong { len: emoji.len() });
        }
    }
```

- [ ] **Step 4: Run test to verify it passes**

```
nix develop --command cargo test -p sunset-core decode_rejects_oversized_reaction_emoji
```

Expected: PASS.

- [ ] **Step 5: Run the full sunset-core suite**

```
nix develop --command cargo test -p sunset-core
```

Expected: all pass.

- [ ] **Step 6: Commit**

```bash
git add crates/sunset-core/src/message.rs
git commit -m "$(cat <<'EOF'
sunset-core: enforce 64-byte emoji cap in decode_message

Defensive check so a peer cannot craft a Reaction entry with an
oversized emoji that bypasses our compose_reaction validation.
Returns Error::EmojiTooLong { len } matching the compose-side error.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task A5: Re-export `compose_reaction` and `ReactionAction` from `lib.rs`

**Files:**
- Modify: `crates/sunset-core/src/lib.rs`

- [ ] **Step 1: Update re-exports**

In `crates/sunset-core/src/lib.rs`, change the `crypto::envelope` re-export and the `message` re-export to:

```rust
pub use crypto::envelope::{EncryptedMessage, MessageBody, ReactionAction, SignedMessage};
```

```rust
pub use message::{
    ComposedMessage, DecodedMessage, compose_message, compose_reaction, compose_receipt,
    compose_text, decode_message,
};
```

- [ ] **Step 2: Verify the crate still builds**

```
nix develop --command cargo build -p sunset-core
```

Expected: clean build.

- [ ] **Step 3: Commit**

```bash
git add crates/sunset-core/src/lib.rs
git commit -m "$(cat <<'EOF'
sunset-core: re-export compose_reaction and ReactionAction

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Phase B — sunset-core: `ReactionTracker` (pure helpers + spawn entrypoint)

### Task B1: Create `reactions.rs` skeleton with type definitions

**Files:**
- Create: `crates/sunset-core/src/reactions.rs`
- Modify: `crates/sunset-core/src/lib.rs`

- [ ] **Step 1: Create the file with types only**

Create `crates/sunset-core/src/reactions.rs`:

```rust
//! Reaction tracker: platform-agnostic chat-semantics layer over the
//! room's `<room_fp>/msg/` store namespace. Filters incoming entries
//! down to `MessageBody::Reaction` events, applies LWW per
//! `(author, target, emoji)` keyed on `(sent_at_ms, value_hash)`, and
//! fires whole-snapshot callbacks per affected target on debounced
//! state changes. Mirrors the shape of `crate::membership` so wasm,
//! TUI, and any future surface plug in via the same callback slot
//! pattern.

use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap};
use std::rc::Rc;

use crate::crypto::envelope::{MessageBody, ReactionAction};
use crate::identity::IdentityKey;
use sunset_store::Hash;

/// Per-target snapshot: emoji → set of authors currently reacting with
/// that emoji. Empty inner set means no live reactions for the emoji
/// (the emoji entry should be omitted by `derive_snapshot`).
pub type ReactionSnapshot = HashMap<String, BTreeSet<IdentityKey>>;

/// Stable signature of a snapshot used for debounce. Sorted lex on
/// emoji, then on author bytes — semantic equality, not allocation
/// identity.
pub type ReactionSig = Vec<(String, Vec<Vec<u8>>)>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ReactionEntry {
    pub action: ReactionAction,
    pub sent_at_ms: u64,
    pub value_hash: Hash,
}

/// One decoded reaction event. Built by `spawn_reaction_tracker` from
/// each decoded `MessageBody::Reaction` and fed into `apply_event`.
#[derive(Clone, Debug)]
pub struct ReactionEvent {
    pub author: IdentityKey,
    pub target: Hash,
    pub emoji: String,
    pub action: ReactionAction,
    pub sent_at_ms: u64,
    pub value_hash: Hash,
}

/// In-memory per-tracker state. `target → emoji → author → entry`.
pub(crate) type ReactionState =
    HashMap<Hash, HashMap<String, HashMap<IdentityKey, ReactionEntry>>>;

/// Callback fired with `(target, snapshot)` whenever the snapshot for
/// `target` changes (per `reactions_signature` debounce).
pub type ReactionsCallback = Box<dyn Fn(&Hash, &ReactionSnapshot)>;

pub type ReactionsCallbackSlot = Rc<RefCell<Option<ReactionsCallback>>>;

/// Shared mutable handles between the tracker task and the host's
/// public API. Cloneable so the host (e.g. `Client`) can keep its own
/// handle alongside the spawned task's.
#[derive(Clone, Default)]
pub struct ReactionHandles {
    pub on_reactions_changed: ReactionsCallbackSlot,
    /// Per-target last-fired snapshot signature. Cleared when the host
    /// re-registers the callback so the next event refires the current
    /// state for that target.
    pub last_target_signatures: Rc<RefCell<HashMap<Hash, ReactionSig>>>,
}
```

- [ ] **Step 2: Wire `pub mod reactions;` and re-export**

In `crates/sunset-core/src/lib.rs`, add `pub mod reactions;` to the module list (alphabetical, after `membership`).

- [ ] **Step 3: Verify the crate builds**

```
nix develop --command cargo build -p sunset-core
```

Expected: clean build. No tests yet — pure types only.

- [ ] **Step 4: Commit**

```bash
git add crates/sunset-core/src/reactions.rs crates/sunset-core/src/lib.rs
git commit -m "$(cat <<'EOF'
sunset-core: scaffold reactions module with type definitions

ReactionEvent, ReactionSnapshot, ReactionSig, ReactionHandles, plus
internal ReactionState. Pure types — apply_event / derive_snapshot /
spawn_reaction_tracker land in subsequent tasks.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task B2: Implement `apply_event` (LWW)

**Files:**
- Modify: `crates/sunset-core/src/reactions.rs`

- [ ] **Step 1: Write the failing tests**

Append to `crates/sunset-core/src/reactions.rs`:

```rust
#[cfg(test)]
mod apply_event_tests {
    use super::*;
    use rand_core::OsRng;

    fn alice() -> IdentityKey {
        crate::identity::Identity::generate(&mut OsRng).public()
    }

    fn bob() -> IdentityKey {
        crate::identity::Identity::generate(&mut OsRng).public()
    }

    fn h(b: u8) -> Hash {
        let arr = [b; 32];
        Hash::from(arr)
    }

    #[test]
    fn apply_event_inserts_first_event() {
        let mut state = ReactionState::new();
        let target = h(1);
        let alice = alice();
        let changed = apply_event(
            &mut state,
            ReactionEvent {
                author: alice.clone(),
                target,
                emoji: "👍".to_owned(),
                action: ReactionAction::Add,
                sent_at_ms: 100,
                value_hash: h(10),
            },
        );
        assert!(changed, "first event should mark target as changed");
        let snap = derive_snapshot(&state, &target);
        let alice_set = snap.get("👍").unwrap();
        assert!(alice_set.contains(&alice));
    }

    #[test]
    fn apply_event_lww_later_timestamp_wins() {
        let mut state = ReactionState::new();
        let target = h(1);
        let alice = alice();
        // Add at t=100
        apply_event(
            &mut state,
            ReactionEvent {
                author: alice.clone(),
                target,
                emoji: "👍".to_owned(),
                action: ReactionAction::Add,
                sent_at_ms: 100,
                value_hash: h(10),
            },
        );
        // Remove at t=200 — later, wins.
        apply_event(
            &mut state,
            ReactionEvent {
                author: alice.clone(),
                target,
                emoji: "👍".to_owned(),
                action: ReactionAction::Remove,
                sent_at_ms: 200,
                value_hash: h(20),
            },
        );
        let snap = derive_snapshot(&state, &target);
        assert!(
            snap.get("👍").map(|s| s.is_empty()).unwrap_or(true),
            "Remove at later timestamp should evict author"
        );
    }

    #[test]
    fn apply_event_lww_earlier_timestamp_loses() {
        let mut state = ReactionState::new();
        let target = h(1);
        let alice = alice();
        // Add at t=200
        apply_event(
            &mut state,
            ReactionEvent {
                author: alice.clone(),
                target,
                emoji: "👍".to_owned(),
                action: ReactionAction::Add,
                sent_at_ms: 200,
                value_hash: h(10),
            },
        );
        // Stale Remove at t=100 — earlier, loses.
        let changed = apply_event(
            &mut state,
            ReactionEvent {
                author: alice.clone(),
                target,
                emoji: "👍".to_owned(),
                action: ReactionAction::Remove,
                sent_at_ms: 100,
                value_hash: h(20),
            },
        );
        let snap = derive_snapshot(&state, &target);
        assert!(snap.get("👍").unwrap().contains(&alice), "stale Remove must not evict");
        // changed may be true (caller does signature compare anyway).
        let _ = changed;
    }

    #[test]
    fn apply_event_value_hash_breaks_timestamp_tie() {
        let mut state = ReactionState::new();
        let target = h(1);
        let alice = alice();
        // Two events at same sent_at_ms — value_hash decides.
        let lower = h(0x05);
        let higher = h(0x50);
        apply_event(
            &mut state,
            ReactionEvent {
                author: alice.clone(),
                target,
                emoji: "👍".to_owned(),
                action: ReactionAction::Add,
                sent_at_ms: 100,
                value_hash: lower,
            },
        );
        apply_event(
            &mut state,
            ReactionEvent {
                author: alice.clone(),
                target,
                emoji: "👍".to_owned(),
                action: ReactionAction::Remove,
                sent_at_ms: 100,
                value_hash: higher,
            },
        );
        let snap = derive_snapshot(&state, &target);
        assert!(
            snap.get("👍").map(|s| s.is_empty()).unwrap_or(true),
            "higher value_hash at same timestamp should win (Remove evicts)"
        );
    }

    #[test]
    fn apply_event_independent_authors_coexist() {
        let mut state = ReactionState::new();
        let target = h(1);
        let alice = alice();
        let bob = bob();
        apply_event(
            &mut state,
            ReactionEvent {
                author: alice.clone(),
                target,
                emoji: "👍".to_owned(),
                action: ReactionAction::Add,
                sent_at_ms: 100,
                value_hash: h(10),
            },
        );
        apply_event(
            &mut state,
            ReactionEvent {
                author: bob.clone(),
                target,
                emoji: "👍".to_owned(),
                action: ReactionAction::Add,
                sent_at_ms: 100,
                value_hash: h(11),
            },
        );
        let snap = derive_snapshot(&state, &target);
        let set = snap.get("👍").unwrap();
        assert!(set.contains(&alice));
        assert!(set.contains(&bob));
        assert_eq!(set.len(), 2);
    }

    #[test]
    fn apply_event_independent_emoji_coexist() {
        let mut state = ReactionState::new();
        let target = h(1);
        let alice = alice();
        apply_event(
            &mut state,
            ReactionEvent {
                author: alice.clone(),
                target,
                emoji: "👍".to_owned(),
                action: ReactionAction::Add,
                sent_at_ms: 100,
                value_hash: h(10),
            },
        );
        apply_event(
            &mut state,
            ReactionEvent {
                author: alice.clone(),
                target,
                emoji: "🎉".to_owned(),
                action: ReactionAction::Add,
                sent_at_ms: 101,
                value_hash: h(11),
            },
        );
        let snap = derive_snapshot(&state, &target);
        assert!(snap.get("👍").unwrap().contains(&alice));
        assert!(snap.get("🎉").unwrap().contains(&alice));
    }

}
```

- [ ] **Step 2: Run tests to verify they fail**

```
nix develop --command cargo test -p sunset-core apply_event
```

Expected: compile errors — `apply_event` and `derive_snapshot` undefined.

- [ ] **Step 3: Implement `apply_event` and a temporary `derive_snapshot` stub**

Append to `crates/sunset-core/src/reactions.rs` (above the `#[cfg(test)]`):

```rust
/// Apply one event to in-memory state. The new entry replaces an
/// existing entry for `(author, target, emoji)` iff `(sent_at_ms,
/// value_hash)` of the new entry is strictly greater than the existing
/// entry's pair. Returns `true` if the snapshot for `event.target`
/// might have changed; the caller still does a signature comparison
/// to decide whether to fire the callback (so `true` is safe to
/// over-report).
pub fn apply_event(state: &mut ReactionState, event: ReactionEvent) -> bool {
    let by_emoji = state.entry(event.target).or_default();
    let by_author = by_emoji.entry(event.emoji.clone()).or_default();
    let new_entry = ReactionEntry {
        action: event.action,
        sent_at_ms: event.sent_at_ms,
        value_hash: event.value_hash,
    };
    match by_author.get(&event.author) {
        Some(existing) => {
            let existing_key = (existing.sent_at_ms, existing.value_hash);
            let new_key = (new_entry.sent_at_ms, new_entry.value_hash);
            if new_key > existing_key {
                by_author.insert(event.author, new_entry);
                true
            } else {
                false
            }
        }
        None => {
            by_author.insert(event.author, new_entry);
            true
        }
    }
}

/// Render the current snapshot for one target. Authors whose latest
/// LWW entry is `Remove` are omitted; emoji entries with no remaining
/// authors are omitted.
pub fn derive_snapshot(state: &ReactionState, target: &Hash) -> ReactionSnapshot {
    let mut out = ReactionSnapshot::new();
    let Some(by_emoji) = state.get(target) else {
        return out;
    };
    for (emoji, by_author) in by_emoji {
        let mut authors = BTreeSet::new();
        for (author, entry) in by_author {
            if entry.action == ReactionAction::Add {
                authors.insert(author.clone());
            }
        }
        if !authors.is_empty() {
            out.insert(emoji.clone(), authors);
        }
    }
    out
}
```

Note: `IdentityKey` needs to derive `Ord` for `BTreeSet`, and `Hash` from sunset_store needs `From<[u8; 32]>` for the test helper. Check both before running.

- [ ] **Step 4: Run tests to verify they pass**

```
nix develop --command cargo test -p sunset-core apply_event
```

Expected: 6 tests pass. If `IdentityKey: Ord` is missing, add the derives in `crates/sunset-core/src/identity.rs` (likely already present — verify).

- [ ] **Step 5: Commit**

```bash
git add crates/sunset-core/src/reactions.rs
git commit -m "$(cat <<'EOF'
sunset-core: implement apply_event + derive_snapshot for reactions

Pure helpers, fully covered by unit tests. apply_event runs LWW per
(author, target, emoji) keyed on (sent_at_ms, value_hash);
derive_snapshot omits authors whose latest entry is Remove.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task B3: Implement `reactions_signature` (debounce key)

**Files:**
- Modify: `crates/sunset-core/src/reactions.rs`

- [ ] **Step 1: Write the failing tests**

Append to `crates/sunset-core/src/reactions.rs` (in a new `#[cfg(test)] mod signature_tests` block):

```rust
#[cfg(test)]
mod signature_tests {
    use super::*;
    use rand_core::OsRng;

    fn alice() -> IdentityKey {
        crate::identity::Identity::generate(&mut OsRng).public()
    }

    #[test]
    fn signature_equal_for_equivalent_snapshots() {
        let mut a = ReactionSnapshot::new();
        let mut b = ReactionSnapshot::new();
        let alice = alice();
        a.entry("👍".to_owned()).or_default().insert(alice.clone());
        b.entry("👍".to_owned()).or_default().insert(alice.clone());
        assert_eq!(reactions_signature(&a), reactions_signature(&b));
    }

    #[test]
    fn signature_changes_when_emoji_added() {
        let mut a = ReactionSnapshot::new();
        let alice = alice();
        a.entry("👍".to_owned()).or_default().insert(alice.clone());
        let s1 = reactions_signature(&a);
        a.entry("🎉".to_owned()).or_default().insert(alice.clone());
        let s2 = reactions_signature(&a);
        assert_ne!(s1, s2);
    }

    #[test]
    fn signature_changes_when_author_added() {
        let mut a = ReactionSnapshot::new();
        let alice = alice();
        let bob = alice(); // distinct identity
        a.entry("👍".to_owned()).or_default().insert(alice);
        let s1 = reactions_signature(&a);
        a.entry("👍".to_owned()).or_default().insert(bob);
        let s2 = reactions_signature(&a);
        assert_ne!(s1, s2);
    }

    #[test]
    fn signature_stable_under_iteration_order() {
        // HashMap/HashSet iteration order is unspecified; the signature
        // sort guarantees equal snapshots produce equal signatures.
        let alice = alice();
        let bob = alice();
        let carol = alice();
        let mut snap = ReactionSnapshot::new();
        for author in [alice.clone(), bob.clone(), carol.clone()] {
            snap.entry("👍".to_owned()).or_default().insert(author);
        }
        let s1 = reactions_signature(&snap);
        // Build a fresh snapshot with the same content and confirm
        // equality despite potential allocation ordering differences.
        let mut snap2 = ReactionSnapshot::new();
        for author in [carol, alice, bob] {
            snap2.entry("👍".to_owned()).or_default().insert(author);
        }
        let s2 = reactions_signature(&snap2);
        assert_eq!(s1, s2);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

```
nix develop --command cargo test -p sunset-core signature_tests
```

Expected: compile errors — `reactions_signature` undefined.

- [ ] **Step 3: Implement `reactions_signature`**

Append to `crates/sunset-core/src/reactions.rs` (next to `derive_snapshot`):

```rust
/// Stable signature of a snapshot used for debounce. Sorted lex on
/// emoji, then on author key bytes. Equal snapshots produce equal
/// signatures regardless of HashMap/BTreeSet iteration order quirks.
pub fn reactions_signature(snapshot: &ReactionSnapshot) -> ReactionSig {
    let mut emoji_keys: Vec<&String> = snapshot.keys().collect();
    emoji_keys.sort();
    emoji_keys
        .into_iter()
        .map(|emoji| {
            let mut authors: Vec<Vec<u8>> = snapshot[emoji]
                .iter()
                .map(|k| k.as_bytes().to_vec())
                .collect();
            authors.sort();
            (emoji.clone(), authors)
        })
        .collect()
}
```

- [ ] **Step 4: Run tests to verify they pass**

```
nix develop --command cargo test -p sunset-core signature_tests
```

Expected: 4 tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/sunset-core/src/reactions.rs
git commit -m "$(cat <<'EOF'
sunset-core: implement reactions_signature for debounce

Sorts emoji + author bytes so equal snapshots produce equal
signatures regardless of HashMap iteration order.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task B4: Implement `spawn_reaction_tracker` and integration test

**Files:**
- Modify: `crates/sunset-core/src/reactions.rs`
- Create: `crates/sunset-core/tests/reactions_tracker.rs`

- [ ] **Step 1: Write the failing integration test**

Create `crates/sunset-core/tests/reactions_tracker.rs`:

```rust
//! End-to-end: spawn a reaction tracker over a MemoryStore, write
//! Reaction entries, observe whole-snapshot callbacks per logical
//! change.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use rand_core::OsRng;
use sunset_core::crypto::constants::test_fast_params;
use sunset_core::reactions::{
    ReactionHandles, ReactionSnapshot, spawn_reaction_tracker,
};
use sunset_core::{Identity, ReactionAction, Room, compose_reaction};
use sunset_store::Store as _;

#[tokio::test(flavor = "current_thread")]
async fn tracker_fires_on_alice_reaction_then_remove() {
    let alice = Identity::generate(&mut OsRng);
    let bob = Identity::generate(&mut OsRng);
    let room = Room::open_with_params("general", &test_fast_params()).unwrap();
    let store = Arc::new(sunset_store_memory::MemoryStore::with_accept_all());

    // Bob's tracker watches the room.
    let handles = ReactionHandles::default();
    let observed: Rc<RefCell<Vec<(sunset_store::Hash, ReactionSnapshot)>>> =
        Rc::new(RefCell::new(Vec::new()));
    let observed_cb = observed.clone();
    *handles.on_reactions_changed.borrow_mut() = Some(Box::new(move |target, snapshot| {
        observed_cb.borrow_mut().push((*target, snapshot.clone()));
    }));
    spawn_reaction_tracker(
        store.clone(),
        room.clone(),
        room.fingerprint().to_hex(),
        handles.clone(),
    );

    // Alice composes a reaction targeting an arbitrary message hash.
    let target: sunset_store::Hash = blake3::hash(b"target message").into();

    let composed_add = compose_reaction(
        &alice,
        &room,
        0,
        100,
        target,
        "👍",
        ReactionAction::Add,
        &mut OsRng,
    )
    .unwrap();
    store
        .insert(composed_add.entry, Some(composed_add.block))
        .await
        .unwrap();

    // Yield to let the spawned task drain the subscription.
    for _ in 0..10 {
        tokio::task::yield_now().await;
        if !observed.borrow().is_empty() {
            break;
        }
    }
    assert_eq!(observed.borrow().len(), 1, "tracker should fire once for Add");
    let (fired_target, fired_snapshot) = observed.borrow()[0].clone();
    assert_eq!(fired_target, target);
    let alice_set = fired_snapshot.get("👍").unwrap();
    assert!(alice_set.contains(&alice.public()));

    // Alice removes the reaction.
    let composed_remove = compose_reaction(
        &alice,
        &room,
        0,
        200,
        target,
        "👍",
        ReactionAction::Remove,
        &mut OsRng,
    )
    .unwrap();
    store
        .insert(composed_remove.entry, Some(composed_remove.block))
        .await
        .unwrap();

    for _ in 0..10 {
        tokio::task::yield_now().await;
        if observed.borrow().len() >= 2 {
            break;
        }
    }
    assert_eq!(observed.borrow().len(), 2, "tracker should fire again for Remove");
    let (_, fired_snapshot_2) = observed.borrow()[1].clone();
    assert!(
        fired_snapshot_2.get("👍").map(|s| s.is_empty()).unwrap_or(true),
        "Remove should yield an empty snapshot for 👍"
    );

    // Suppress unused-variable warnings for bob (kept for symmetry with
    // future tests where bob also reacts).
    let _ = bob;
}

#[tokio::test(flavor = "current_thread")]
async fn tracker_debounces_duplicate_state() {
    // Two consecutive Adds with different timestamps but same outcome
    // should fire twice (signature changes only on outcome change), but
    // re-applying the same event twice (e.g., from Replay::All) must
    // NOT fire a redundant callback.
    let alice = Identity::generate(&mut OsRng);
    let room = Room::open_with_params("general", &test_fast_params()).unwrap();
    let store = Arc::new(sunset_store_memory::MemoryStore::with_accept_all());

    let handles = ReactionHandles::default();
    let observed: Rc<RefCell<Vec<sunset_store::Hash>>> = Rc::new(RefCell::new(Vec::new()));
    let observed_cb = observed.clone();
    *handles.on_reactions_changed.borrow_mut() = Some(Box::new(move |target, _snapshot| {
        observed_cb.borrow_mut().push(*target);
    }));
    spawn_reaction_tracker(
        store.clone(),
        room.clone(),
        room.fingerprint().to_hex(),
        handles.clone(),
    );

    let target: sunset_store::Hash = blake3::hash(b"target").into();
    let composed = compose_reaction(
        &alice,
        &room,
        0,
        100,
        target,
        "👍",
        ReactionAction::Add,
        &mut OsRng,
    )
    .unwrap();
    // Insert the same entry twice (the second insert is a no-op at the
    // store level — same value_hash); the tracker should also not
    // double-fire.
    store
        .insert(composed.entry.clone(), Some(composed.block.clone()))
        .await
        .unwrap();
    let _ = store
        .insert(composed.entry.clone(), Some(composed.block.clone()))
        .await;

    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
    assert_eq!(observed.borrow().len(), 1, "duplicate insert must not double-fire");
}
```

- [ ] **Step 2: Run test to verify it fails**

```
nix develop --command cargo test -p sunset-core --test reactions_tracker
```

Expected: compile errors — `spawn_reaction_tracker` undefined.

- [ ] **Step 3: Implement `spawn_reaction_tracker`**

Append to `crates/sunset-core/src/reactions.rs`:

```rust
use bytes::Bytes;
use futures::StreamExt;
use sunset_store::{Filter, Replay, Store};

use crate::crypto::room::Room;
use crate::message::decode_message;

/// Spawn the reaction tracker. Subscribes to the room's
/// `<room_fp>/msg/` namespace, decodes each entry, filters down to
/// `MessageBody::Reaction` events, applies them, and fires
/// `on_reactions_changed` per debounced per-target snapshot change.
///
/// Runs forever (host-process / page lifetime). The store subscription
/// is `Replay::All` so historical reactions are re-folded on startup.
pub fn spawn_reaction_tracker<S: Store + 'static>(
    store: std::sync::Arc<S>,
    room: Room,
    room_fp_hex: String,
    handles: ReactionHandles,
) {
    sunset_sync::spawn::spawn_local(async move {
        let prefix = format!("{room_fp_hex}/msg/");
        let filter = Filter::NamePrefix(Bytes::from(prefix));
        let mut sub = match store.subscribe(filter, Replay::All).await {
            Ok(s) => s,
            Err(e) => {
                eprintln!("ReactionTracker: subscribe failed: {e}");
                return;
            }
        };

        let mut state = ReactionState::new();

        while let Some(ev) = sub.next().await {
            let entry = match ev {
                Ok(sunset_store::Event::Inserted(e)) => e,
                Ok(sunset_store::Event::Replaced { new, .. }) => new,
                Ok(_) => continue,
                Err(e) => {
                    eprintln!("ReactionTracker: store event error: {e}");
                    continue;
                }
            };
            let block = match store.get_content(&entry.value_hash).await {
                Ok(Some(b)) => b,
                Ok(None) => continue,
                Err(e) => {
                    eprintln!("ReactionTracker: get_content failed: {e}");
                    continue;
                }
            };
            let decoded = match decode_message(&room, &entry, &block) {
                Ok(d) => d,
                Err(e) => {
                    eprintln!("ReactionTracker: decode_message failed: {e}");
                    continue;
                }
            };
            let MessageBody::Reaction {
                for_value_hash,
                emoji,
                action,
            } = decoded.body
            else {
                continue;
            };
            let event = ReactionEvent {
                author: decoded.author_key,
                target: for_value_hash,
                emoji,
                action,
                sent_at_ms: decoded.sent_at_ms,
                value_hash: decoded.value_hash,
            };
            let target = event.target;
            if !apply_event(&mut state, event) {
                continue;
            }
            let snapshot = derive_snapshot(&state, &target);
            let new_sig = reactions_signature(&snapshot);
            let mut sigs = handles.last_target_signatures.borrow_mut();
            let prev = sigs.get(&target);
            if prev == Some(&new_sig) {
                continue;
            }
            sigs.insert(target, new_sig);
            drop(sigs);
            if let Some(cb) = handles.on_reactions_changed.borrow().as_ref() {
                cb(&target, &snapshot);
            }
        }
    });
}
```

- [ ] **Step 4: Run integration test to verify it passes**

```
nix develop --command cargo test -p sunset-core --test reactions_tracker
```

Expected: 2 tests pass.

- [ ] **Step 5: Run full sunset-core suite**

```
nix develop --command cargo test -p sunset-core --all-features
```

Expected: all pass.

- [ ] **Step 6: Run clippy**

```
nix develop --command cargo clippy -p sunset-core --all-features --all-targets -- -D warnings
```

Expected: no warnings.

- [ ] **Step 7: Commit**

```bash
git add crates/sunset-core/src/reactions.rs crates/sunset-core/tests/reactions_tracker.rs
git commit -m "$(cat <<'EOF'
sunset-core: implement spawn_reaction_tracker

Self-driven over its own <room_fp>/msg/ subscription. Mirrors
membership::spawn_tracker shape: hosts pass in a ReactionHandles slot
and the spawned task fires on_reactions_changed on debounced per-
target snapshot changes. Replay::All-safe via signature debounce.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task B5: Re-export reactions API from `lib.rs`

**Files:**
- Modify: `crates/sunset-core/src/lib.rs`

- [ ] **Step 1: Add re-exports**

In `crates/sunset-core/src/lib.rs`, add below the existing re-exports:

```rust
pub use reactions::{
    ReactionEvent, ReactionHandles, ReactionSnapshot, ReactionsCallback,
    apply_event, derive_snapshot, reactions_signature, spawn_reaction_tracker,
};
```

- [ ] **Step 2: Verify the crate builds**

```
nix develop --command cargo build -p sunset-core
```

Expected: clean build.

- [ ] **Step 3: Commit**

```bash
git add crates/sunset-core/src/lib.rs
git commit -m "$(cat <<'EOF'
sunset-core: re-export reactions public API

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Phase C — sunset-web-wasm: bridge wiring

### Task C1: Add `reaction_handles` to `Client` and spawn tracker in `new`

**Files:**
- Modify: `crates/sunset-web-wasm/src/client.rs`

- [ ] **Step 1: Add the field**

In `crates/sunset-web-wasm/src/client.rs`, update imports at top:

```rust
use sunset_core::membership::{Member, TrackerHandles};
use sunset_core::reactions::ReactionHandles;
use sunset_core::{Ed25519Verifier, Identity, MessageBody, ReactionAction, Room};
```

Add a field to the `Client` struct (after `tracker_handles`):

```rust
reaction_handles: ReactionHandles,
```

- [ ] **Step 2: Initialize and spawn the tracker in `Client::new`**

In `Client::new`, after the supervisor spawn block and before the `Ok(Client { ... })` return, add:

```rust
let reaction_handles = ReactionHandles::default();
sunset_core::spawn_reaction_tracker(
    store.clone(),
    (*room).clone(),
    room.fingerprint().to_hex(),
    reaction_handles.clone(),
);
```

Then add `reaction_handles,` to the `Client { ... }` struct construction.

- [ ] **Step 3: Verify the crate builds**

```
nix develop --command cargo build -p sunset-web-wasm --target wasm32-unknown-unknown
```

Expected: clean build. (Native build also works:
`nix develop --command cargo build -p sunset-web-wasm`.)

- [ ] **Step 4: Commit**

```bash
git add crates/sunset-web-wasm/src/client.rs
git commit -m "$(cat <<'EOF'
sunset-web-wasm: spawn ReactionTracker in Client::new

ReactionHandles slot lives on Client; the spawned tracker drives it
from the room's <fp>/msg/ store namespace. Callback wiring lands in
the next task.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task C2: JS marshaling helpers for reactions

**Files:**
- Create: `crates/sunset-web-wasm/src/reactions.rs`
- Modify: `crates/sunset-web-wasm/src/lib.rs`

- [ ] **Step 1: Create the marshaling module**

Create `crates/sunset-web-wasm/src/reactions.rs`:

```rust
//! JS marshaling for the reaction tracker's snapshot callbacks.

use js_sys::{Array, Map, Set};
use sunset_core::ReactionSnapshot;
use sunset_store::Hash;
use wasm_bindgen::prelude::*;

/// Build the JS payload object dispatched to the FE's
/// `on_reactions_changed` callback. Shape:
///
/// ```ts
/// {
///   target_hex: string,
///   reactions: Map<emoji_string, Set<author_pubkey_hex>>
/// }
/// ```
pub fn snapshot_to_js(target: &Hash, snapshot: &ReactionSnapshot) -> JsValue {
    let map = Map::new();
    for (emoji, authors) in snapshot {
        let set = Set::new(&JsValue::UNDEFINED);
        for author in authors {
            set.add(&JsValue::from_str(&hex::encode(author.as_bytes())));
        }
        map.set(&JsValue::from_str(emoji), &set.into());
    }
    let obj = js_sys::Object::new();
    let _ = js_sys::Reflect::set(
        &obj,
        &JsValue::from_str("target_hex"),
        &JsValue::from_str(&target.to_hex()),
    );
    let _ = js_sys::Reflect::set(&obj, &JsValue::from_str("reactions"), &map.into());
    obj.into()
}

/// Push as a callback args array of length 2 (target_hex, reactions_map)
/// — alternative shape if we ever want to switch the FE to positional
/// args. Currently unused; present so the FFI signature can pivot
/// without rewiring marshaling.
pub fn snapshot_to_args(target: &Hash, snapshot: &ReactionSnapshot) -> Array {
    let arr = Array::new();
    arr.push(&JsValue::from_str(&target.to_hex()));
    let map = Map::new();
    for (emoji, authors) in snapshot {
        let set = Set::new(&JsValue::UNDEFINED);
        for author in authors {
            set.add(&JsValue::from_str(&hex::encode(author.as_bytes())));
        }
        map.set(&JsValue::from_str(emoji), &set.into());
    }
    arr.push(&map.into());
    arr
}
```

- [ ] **Step 2: Wire `mod reactions;`**

In `crates/sunset-web-wasm/src/lib.rs`, add `mod reactions;` to the module list (alphabetical, after `presence_publisher` or wherever it fits).

- [ ] **Step 3: Verify build**

```
nix develop --command cargo build -p sunset-web-wasm --target wasm32-unknown-unknown
```

Expected: clean build.

- [ ] **Step 4: Commit**

```bash
git add crates/sunset-web-wasm/src/reactions.rs crates/sunset-web-wasm/src/lib.rs
git commit -m "$(cat <<'EOF'
sunset-web-wasm: add reactions JS marshaling helpers

snapshot_to_js builds the { target_hex, reactions: Map<emoji, Set> }
payload the FE on_reactions_changed callback receives.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task C3: `Client::on_reactions_changed`

**Files:**
- Modify: `crates/sunset-web-wasm/src/client.rs`

- [ ] **Step 1: Add the method**

In `crates/sunset-web-wasm/src/client.rs`, add after `on_relay_status_changed`:

```rust
pub fn on_reactions_changed(&self, callback: js_sys::Function) {
    let bridge = move |target: &sunset_store::Hash, snapshot: &sunset_core::ReactionSnapshot| {
        let payload = crate::reactions::snapshot_to_js(target, snapshot);
        let _ = callback.call1(&JsValue::NULL, &payload);
    };
    *self.reaction_handles.on_reactions_changed.borrow_mut() = Some(Box::new(bridge));
    // Clear the per-target debounce signatures so the tracker's next
    // applied event refires the snapshot. (Mirrors the on_members_changed
    // last_signature.clear() pattern.)
    self.reaction_handles.last_target_signatures.borrow_mut().clear();
}
```

- [ ] **Step 2: Verify build**

```
nix develop --command cargo build -p sunset-web-wasm --target wasm32-unknown-unknown
```

Expected: clean build.

- [ ] **Step 3: Commit**

```bash
git add crates/sunset-web-wasm/src/client.rs
git commit -m "$(cat <<'EOF'
sunset-web-wasm: add Client::on_reactions_changed

Wraps a JS callback as the ReactionsCallback the tracker invokes.
Clears per-target signatures on register so the next event refires
current state for that target.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task C4: `Client::send_reaction`

**Files:**
- Modify: `crates/sunset-web-wasm/src/client.rs`

- [ ] **Step 1: Add the method**

In `crates/sunset-web-wasm/src/client.rs`, add after `send_message`:

```rust
pub async fn send_reaction(
    &self,
    target_value_hash_hex: String,
    emoji: String,
    action: String,
    sent_at_ms: f64,
    nonce_seed: Vec<u8>,
) -> Result<(), JsError> {
    use sunset_store::Store as _;

    let action = match action.as_str() {
        "add" => ReactionAction::Add,
        "remove" => ReactionAction::Remove,
        other => {
            return Err(JsError::new(&format!(
                "send_reaction: action must be \"add\" or \"remove\", got {other:?}"
            )));
        }
    };
    let target_bytes = hex::decode(&target_value_hash_hex)
        .map_err(|e| JsError::new(&format!("send_reaction: bad target hex: {e}")))?;
    if target_bytes.len() != 32 {
        return Err(JsError::new("send_reaction: target hex must decode to 32 bytes"));
    }
    let mut target_arr = [0u8; 32];
    target_arr.copy_from_slice(&target_bytes);
    let target: sunset_store::Hash = target_arr.into();

    let nonce_seed_arr: [u8; 32] = nonce_seed
        .as_slice()
        .try_into()
        .map_err(|_| JsError::new("nonce_seed must be 32 bytes"))?;
    let mut rng = rand_chacha::ChaCha20Rng::from_seed(nonce_seed_arr);

    let composed = sunset_core::compose_reaction(
        &self.identity,
        &self.room,
        0u64,
        sent_at_ms as u64,
        target,
        &emoji,
        action,
        &mut rng,
    )
    .map_err(|e| JsError::new(&format!("compose_reaction: {e}")))?;

    self.store
        .insert(composed.entry, Some(composed.block))
        .await
        .map_err(|e| JsError::new(&format!("store insert: {e}")))?;

    Ok(())
}
```

- [ ] **Step 2: Verify build**

```
nix develop --command cargo build -p sunset-web-wasm --target wasm32-unknown-unknown
```

Expected: clean build.

- [ ] **Step 3: Run wasm clippy**

```
nix develop --command cargo clippy -p sunset-web-wasm --target wasm32-unknown-unknown -- -D warnings
```

Expected: no warnings.

- [ ] **Step 4: Commit**

```bash
git add crates/sunset-web-wasm/src/client.rs
git commit -m "$(cat <<'EOF'
sunset-web-wasm: add Client::send_reaction

Parses the action string + target hex, derives ChaCha20Rng from the
JS-supplied nonce seed (matching send_message), composes via
compose_reaction, and inserts to the local store. The tracker picks
the entry up via the same subscription as peer reactions.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task C5: Bridge integration test (Alice ↔ Bob)

**Files:**
- Create: `crates/sunset-core/tests/two_peer_reaction.rs` (mirrors the existing `two_peer_message.rs` and `receipts.rs` patterns; lives in sunset-core because the bridge test path uses `compose_reaction` + `decode_message` directly without needing the wasm bridge runtime).

- [ ] **Step 1: Write the failing integration test**

Create `crates/sunset-core/tests/two_peer_reaction.rs`:

```rust
//! End-to-end: Alice reacts 👍 on a message; Bob's tracker sees the
//! snapshot. Alice removes; Bob's tracker sees the empty snapshot.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use rand_core::OsRng;
use sunset_core::crypto::constants::test_fast_params;
use sunset_core::reactions::{ReactionHandles, ReactionSnapshot, spawn_reaction_tracker};
use sunset_core::{Identity, ReactionAction, Room, compose_reaction, compose_text};
use sunset_store::Store as _;

#[tokio::test(flavor = "current_thread")]
async fn reaction_round_trip_between_two_identities() {
    let alice = Identity::generate(&mut OsRng);
    let bob = Identity::generate(&mut OsRng);
    let room = Room::open_with_params("general", &test_fast_params()).unwrap();

    let alice_store = Arc::new(sunset_store_memory::MemoryStore::with_accept_all());
    let bob_store = Arc::new(sunset_store_memory::MemoryStore::with_accept_all());

    // Bob runs a tracker on his store.
    let bob_handles = ReactionHandles::default();
    let observed: Rc<RefCell<Vec<ReactionSnapshot>>> = Rc::new(RefCell::new(Vec::new()));
    let observed_cb = observed.clone();
    *bob_handles.on_reactions_changed.borrow_mut() = Some(Box::new(move |_target, snapshot| {
        observed_cb.borrow_mut().push(snapshot.clone());
    }));
    spawn_reaction_tracker(
        bob_store.clone(),
        room.clone(),
        room.fingerprint().to_hex(),
        bob_handles.clone(),
    );

    // 1. Alice composes a Text and inserts it on her store; sync to bob.
    let text = compose_text(&alice, &room, 0, 1, "hello bob", &mut OsRng).unwrap();
    let target = text.entry.value_hash;
    alice_store
        .insert(text.entry.clone(), Some(text.block.clone()))
        .await
        .unwrap();
    bob_store
        .insert(text.entry.clone(), Some(text.block.clone()))
        .await
        .unwrap();

    // 2. Alice reacts 👍 on her own message.
    let add = compose_reaction(
        &alice,
        &room,
        0,
        100,
        target,
        "👍",
        ReactionAction::Add,
        &mut OsRng,
    )
    .unwrap();
    alice_store
        .insert(add.entry.clone(), Some(add.block.clone()))
        .await
        .unwrap();

    // 3. Sync to Bob.
    bob_store
        .insert(add.entry.clone(), Some(add.block.clone()))
        .await
        .unwrap();

    // 4. Bob's tracker fires.
    for _ in 0..10 {
        tokio::task::yield_now().await;
        if !observed.borrow().is_empty() {
            break;
        }
    }
    assert_eq!(observed.borrow().len(), 1);
    let snap = observed.borrow()[0].clone();
    let alice_set = snap.get("👍").unwrap();
    assert!(alice_set.contains(&alice.public()));

    // 5. Alice removes.
    let remove = compose_reaction(
        &alice,
        &room,
        0,
        200,
        target,
        "👍",
        ReactionAction::Remove,
        &mut OsRng,
    )
    .unwrap();
    alice_store
        .insert(remove.entry.clone(), Some(remove.block.clone()))
        .await
        .unwrap();
    bob_store
        .insert(remove.entry.clone(), Some(remove.block.clone()))
        .await
        .unwrap();

    for _ in 0..10 {
        tokio::task::yield_now().await;
        if observed.borrow().len() >= 2 {
            break;
        }
    }
    assert_eq!(observed.borrow().len(), 2);
    let snap2 = observed.borrow()[1].clone();
    assert!(snap2.get("👍").map(|s| s.is_empty()).unwrap_or(true));

    let _ = bob; // bob is reserved for symmetry — used in expanded versions
}
```

- [ ] **Step 2: Run test to verify it passes**

```
nix develop --command cargo test -p sunset-core --test two_peer_reaction
```

Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/sunset-core/tests/two_peer_reaction.rs
git commit -m "$(cat <<'EOF'
sunset-core: integration test for round-trip reaction Add/Remove

Alice reacts 👍 on her own text message; Bob's tracker observes the
snapshot. Alice removes; Bob's tracker observes the empty snapshot.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Phase D — web/: FFI + state plumbing

### Task D1: Add `emoji-picker-element` dependency through the Nix flake

**Files:**
- Modify: `web/package.json`
- Modify: `flake.nix`

- [ ] **Step 1: Add the dep to package.json**

Edit `web/package.json` and add `"emoji-picker-element": "^1.21.0"` to `dependencies` (the major version is stable; the lowest reasonable target).

- [ ] **Step 2: Update the npm lockfile under nix**

```
nix develop --command npm install --prefix web emoji-picker-element
```

Expected: `web/package-lock.json` updates with the new dep + transitive (the package has no runtime deps, so the lock change is small).

- [ ] **Step 3: Confirm flake build still resolves**

```
nix build .#web --print-out-paths
```

Expected: web build succeeds. If the flake needs an update (e.g., `npmDepsHash` mismatch), follow the flake's existing convention for re-pinning the npm hash. Run any suggested `nix flake check` or `prefetch-npm-deps` step the build prints.

If the flake gates npm fetch through a separate hash-pinned derivation (likely; check `flake.nix` for the web build), update that hash and rebuild until clean.

- [ ] **Step 4: Commit**

```bash
git add web/package.json web/package-lock.json flake.nix
git commit -m "$(cat <<'EOF'
web: add emoji-picker-element dependency

For the reactions full picker. Web component drop-in (~30 KB
minified, MIT, no runtime dep tree). Flake hash bumped so
`nix build .#web` resolves the package without an implicit npm
install step (per CLAUDE.md hermeticity rule).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task D2: FFI bindings on the Gleam side

**Files:**
- Modify: `web/src/sunset_web/sunset.gleam`

- [ ] **Step 1: Add the typed externals**

In `web/src/sunset_web/sunset.gleam`, after the existing `on_message` external block, add:

```gleam
/// Snapshot payload delivered to `on_reactions_changed`. Opaque on the
/// Gleam side; accessors below extract the concrete fields.
pub type IncomingReactionsSnapshot

@external(javascript, "./sunset.ffi.mjs", "reactionsSnapshotTargetHex")
pub fn reactions_snapshot_target_hex(snapshot: IncomingReactionsSnapshot) -> String

/// Returns the snapshot as a `List(#(emoji, List(author_pubkey_hex)))`.
/// The FFI side flattens the JS Map<emoji, Set<author_hex>> into this
/// shape so Gleam doesn't need to interop with Map/Set directly.
@external(javascript, "./sunset.ffi.mjs", "reactionsSnapshotEntries")
pub fn reactions_snapshot_entries(
  snapshot: IncomingReactionsSnapshot,
) -> List(#(String, List(String)))

/// Register the per-target snapshot callback. Fires on initial replay
/// and again whenever the target's reaction state changes.
@external(javascript, "./sunset.ffi.mjs", "onReactionsChanged")
pub fn on_reactions_changed(
  client: ClientHandle,
  callback: fn(IncomingReactionsSnapshot) -> Nil,
) -> Nil

/// Send a reaction event. `action` is "add" or "remove".
@external(javascript, "./sunset.ffi.mjs", "sendReaction")
pub fn send_reaction(
  client: ClientHandle,
  target_hex: String,
  emoji: String,
  action: String,
  sent_at_ms: Int,
  callback: fn(Result(Nil, String)) -> Nil,
) -> Nil
```

- [ ] **Step 2: Verify Gleam build (typed externals only — no JS yet)**

```
nix develop --command bash -c "cd web && gleam build"
```

Expected: build succeeds. The `@external` declarations don't fail at compile time even if the JS side is missing — they fail at runtime if called.

- [ ] **Step 3: Commit**

```bash
git add web/src/sunset_web/sunset.gleam
git commit -m "$(cat <<'EOF'
web: add Gleam FFI declarations for reactions

on_reactions_changed (subscribe), send_reaction, plus opaque
IncomingReactionsSnapshot accessors that the JS side flattens to
List(#(emoji, List(author_hex))) for ergonomic Gleam consumption.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task D3: FFI bindings on the JS side

**Files:**
- Modify: `web/src/sunset_web/sunset.ffi.mjs`

- [ ] **Step 1: Add the JS implementations**

Append to `web/src/sunset_web/sunset.ffi.mjs`:

```javascript
export function onReactionsChanged(client, callback) {
  client.on_reactions_changed((payload) => {
    callback(payload);
  });
}

export function reactionsSnapshotTargetHex(snapshot) {
  return snapshot.target_hex;
}

export function reactionsSnapshotEntries(snapshot) {
  // snapshot.reactions is a Map<emoji, Set<author_hex>>. Flatten into
  // a Gleam list of tuples for ergonomic consumption.
  const out = [];
  for (const [emoji, set] of snapshot.reactions.entries()) {
    out.push([emoji, toList([...set])]);
  }
  return toList(out);
}

export function sendReaction(client, targetHex, emoji, action, sentAtMs, callback) {
  const nonceSeed = window.crypto.getRandomValues(new Uint8Array(32));
  client
    .send_reaction(targetHex, emoji, action, sentAtMs, nonceSeed)
    .then(() => callback(new Ok(undefined)))
    .catch((e) => callback(new GError(String(e?.message ?? e))));
}
```

(`toList`, `Ok`, `GError` are already imported at the top of the file from `../../prelude.mjs` — confirm and add if missing.)

- [ ] **Step 2: Manual smoke check (optional)**

Spin up the dev server and confirm the wasm bindings link without errors:

```
nix develop --command bash -c "cd web && npm run dev"
```

Open the browser console — no errors at load. (We're not exercising the API yet; the Lustre wiring lands in D5.)

- [ ] **Step 3: Commit**

```bash
git add web/src/sunset_web/sunset.ffi.mjs
git commit -m "$(cat <<'EOF'
web: JS FFI implementations for reactions

onReactionsChanged subscribes to the bridge callback.
reactionsSnapshotEntries flattens the Map<emoji, Set<author>> payload
to a Gleam list of tuples. sendReaction generates a fresh nonce seed
each call and proxies to Client.send_reaction.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task D4: Replace Model.reactions shape with bridge-driven snapshot dict

**Files:**
- Modify: `web/src/sunset_web.gleam`

- [ ] **Step 1: Update the `Model` type**

In `web/src/sunset_web.gleam`, change the `reactions` field on the `Model` type:

```gleam
/// Per-target reaction state from the bridge tracker. Whole-snapshot
/// replacement on each `ReactionsChanged` — never partially merged in
/// the FE; the core tracker is the source of truth. Shape:
/// `Dict(target_hex, Dict(emoji, Set(author_pubkey_hex)))`.
reactions: Dict(String, Dict(String, Set(String))),
```

- [ ] **Step 2: Update `init` to start with an empty dict**

In `init`, change `reactions: seed_reactions(),` to:

```gleam
reactions: dict.new(),
```

- [ ] **Step 3: Delete `seed_reactions` and `toggle_reaction`**

Search and delete the now-dead helpers:
- `fn seed_reactions() -> Dict(...)`
- `fn toggle_reaction(rs: List(Reaction), emoji: String) -> List(Reaction)`

(Confirm no other call sites — `grep -n "seed_reactions\|toggle_reaction" web/src` should return empty after deletion.)

- [ ] **Step 4: Drop the `Reaction` import in `sunset_web.gleam`**

The `Reaction` type is now only used by views. Remove it from the `domain.{...}` import list at the top of `sunset_web.gleam`.

- [ ] **Step 5: Verify Gleam build fails (we changed a public type used downstream)**

```
nix develop --command bash -c "cd web && gleam build"
```

Expected: compile errors — `AddReaction` update branch still calls `toggle_reaction` and uses old shape; `messages_with_live_reactions` still does `dict.get(model.reactions, ...) -> List(Reaction)`. This is correct; the next task replaces those.

- [ ] **Step 6 (no commit yet)**

The build is intentionally broken; commit lands at the end of Task D6 after `AddReaction` and view-time conversion are in place.

---

### Task D5: View-time snapshot → `List(Reaction)` conversion

**Files:**
- Modify: `web/src/sunset_web.gleam`

- [ ] **Step 1: Add the conversion helper**

In `web/src/sunset_web.gleam`, add near the existing `find_message` helper:

```gleam
/// Convert a per-target snapshot dict into the `List(Reaction)` shape
/// the chip-row view consumes. `self_pubkey_hex` decides the
/// `by_you` flag; `None` (no client yet) treats every reaction as
/// not-by-you so the UI doesn't lie.
fn snapshot_to_reactions(
  snapshot: Dict(String, Set(String)),
  self_pubkey_hex: Option(String),
) -> List(domain.Reaction) {
  dict.to_list(snapshot)
  |> list.filter_map(fn(pair) {
    let #(emoji, authors) = pair
    case set.size(authors) {
      0 -> Error(Nil)
      n -> {
        let by_you = case self_pubkey_hex {
          Some(me) -> set.contains(authors, me)
          None -> False
        }
        Ok(domain.Reaction(emoji: emoji, count: n, by_you: by_you))
      }
    }
  })
}
```

- [ ] **Step 2: Replace `messages_with_live_reactions` in `room_view`**

In `room_view`, replace:

```gleam
let messages_with_live_reactions =
  list.map(raw_messages, fn(m) {
    case dict.get(model.reactions, m.id) {
      Ok(rs) -> domain.Message(..m, reactions: rs)
      Error(_) -> m
    }
  })
```

with:

```gleam
let self_pubkey_hex = option.map(model.client, fn(c) {
  client_pubkey_hex(c)
})
let messages_with_live_reactions =
  list.map(raw_messages, fn(m) {
    case dict.get(model.reactions, m.id) {
      Ok(snap) -> domain.Message(
        ..m,
        reactions: snapshot_to_reactions(snap, self_pubkey_hex),
      )
      Error(_) -> m
    }
  })
```

- [ ] **Step 3: Add `client_pubkey_hex` accessor**

Bridges through an FFI export. Add to `web/src/sunset_web/sunset.gleam`:

```gleam
@external(javascript, "./sunset.ffi.mjs", "clientPublicKeyHex")
pub fn client_public_key_hex(client: ClientHandle) -> String
```

And to `web/src/sunset_web/sunset.ffi.mjs`:

```javascript
export function clientPublicKeyHex(client) {
  // client.public_key returns Vec<u8>; hex-encode for FE comparison.
  const bytes = client.public_key;
  return [...bytes].map((b) => b.toString(16).padStart(2, "0")).join("");
}
```

In `sunset_web.gleam`, add a private wrapper:

```gleam
fn client_pubkey_hex(c: ClientHandle) -> String {
  sunset.client_public_key_hex(c)
}
```

(Cached: this gets called once per render; if profiles flag it as hot, memoize on first non-None client and store in Model.)

- [ ] **Step 4 (no commit yet)** — buildup continues in D6.

---

### Task D6: `ReactionsChanged` Msg + `AddReaction` rewire

**Files:**
- Modify: `web/src/sunset_web.gleam`

- [ ] **Step 1: Add the new Msg variants**

In the `Msg` enum, replace the existing `AddReaction(String, String)` line with:

```gleam
ReactionsChanged(target: String, snapshot: Dict(String, Set(String)))
ToggleReactionPicker(String)
ToggleReactionEmoji(target: String, emoji: String)
ReactionSent(Result(Nil, String))
```

(Preserve `ToggleReactionPicker` exactly as it is today.)

Replace `AddReaction` everywhere it's referenced (search file). The new variant `ToggleReactionEmoji` carries the same `(message_id, emoji)` payload. Update:
- The view dispatch in `main_panel.view(... on_add_reaction: AddReaction)` to use `ToggleReactionEmoji`.
- Any other call sites (run grep; the picker on-click in views/main_panel.gleam stays unchanged at the parameter name `on_add_reaction`).

- [ ] **Step 2: Implement the `ReactionsChanged` update branch**

Replace the existing `AddReaction(id, emoji) -> { ... }` branch with:

```gleam
ReactionsChanged(target, snapshot) -> {
  #(
    Model(..model, reactions: dict.insert(model.reactions, target, snapshot)),
    effect.none(),
  )
}
ToggleReactionEmoji(target, emoji) -> {
  let self_pubkey_hex_opt = option.map(model.client, fn(c) {
    client_pubkey_hex(c)
  })
  let action = case dict.get(model.reactions, target) {
    Ok(snap) -> case dict.get(snap, emoji), self_pubkey_hex_opt {
      Ok(authors), Some(me) -> case set.contains(authors, me) {
        True -> "remove"
        False -> "add"
      }
      _, _ -> "add"
    }
    Error(_) -> "add"
  }
  let next_model = Model(..model, reacting_to: None)
  let send_effect = case model.client {
    Some(c) ->
      effect.from(fn(dispatch) {
        let now = storage.now_ms()
        sunset.send_reaction(c, target, emoji, action, now, fn(result) {
          dispatch(ReactionSent(result))
        })
      })
    None -> effect.none()
  }
  #(next_model, send_effect)
}
ReactionSent(Ok(_)) -> #(model, effect.none())
ReactionSent(Error(msg)) -> {
  // Bridge insert failed — fall through silently for v1; future work
  // surfaces a toast. Console logging is the bridge's job.
  io.println("send_reaction error: " <> msg)
  #(model, effect.none())
}
```

(Imports: ensure `gleam/io` is imported. `storage.now_ms` exists today; if not, add an FFI returning `Date.now()`.)

- [ ] **Step 3: Verify Gleam build**

```
nix develop --command bash -c "cd web && gleam build"
```

Expected: clean build.

- [ ] **Step 4 (no commit yet)** — wiring init lands in D7.

---

### Task D7: Wire `on_reactions_changed` in `init` effect batch

**Files:**
- Modify: `web/src/sunset_web.gleam`

- [ ] **Step 1: Add the subscription effect**

Find the effect block in `update`'s `ClientReady(client)` branch (or wherever `on_message` / `on_receipt` are wired today). Add:

```gleam
effect.from(fn(dispatch) {
  sunset.on_reactions_changed(client, fn(snapshot_payload) {
    let target = sunset.reactions_snapshot_target_hex(snapshot_payload)
    let entries = sunset.reactions_snapshot_entries(snapshot_payload)
    let inner_dict =
      list.fold(entries, dict.new(), fn(d, pair) {
        let #(emoji, authors) = pair
        dict.insert(d, emoji, set.from_list(authors))
      })
    dispatch(ReactionsChanged(target, inner_dict))
  })
})
```

Batched alongside the existing `on_message` / `on_receipt` subscriptions in the same effect array.

- [ ] **Step 2: Verify Gleam build**

```
nix develop --command bash -c "cd web && gleam build"
```

Expected: clean build.

- [ ] **Step 3: Commit (D4 → D7 land together because they form one logical change)**

```bash
git add web/src/sunset_web.gleam web/src/sunset_web/sunset.gleam web/src/sunset_web/sunset.ffi.mjs
git commit -m "$(cat <<'EOF'
web: replace fixture reactions fold with bridge-driven snapshots

Model.reactions now stores Dict(target_hex, Dict(emoji, Set(author))),
populated by the bridge ReactionTracker via on_reactions_changed.
ToggleReactionEmoji inspects current state and dispatches an "add" or
"remove" send_reaction call. View-time snapshot_to_reactions converts
to the existing domain.Reaction shape so the chip UI is unchanged.

Drops the now-dead seed_reactions and toggle_reaction helpers.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Phase E — web/: emoji-picker-element integration

### Task E1: Picker view module

**Files:**
- Create: `web/src/sunset_web/views/emoji_picker.gleam`

- [ ] **Step 1: Create the picker view**

Create `web/src/sunset_web/views/emoji_picker.gleam`:

```gleam
//// Wraps the `emoji-picker-element` web component as a Lustre element.
//// Lazy-loaded on first picker open; the `register_emoji_picker` FFI
//// dynamically imports the package and registers the custom element.

import lustre/attribute
import lustre/element.{type Element}
import lustre/event

/// Pick callback: the emoji unicode string chosen from the picker.
pub fn view(
  on_pick: fn(String) -> msg,
  on_close: msg,
) -> Element(msg) {
  element.element(
    "emoji-picker",
    [
      attribute.attribute("data-testid", "full-emoji-picker"),
      // emoji-click is the picker's CustomEvent. The decoder pulls
      // event.detail.unicode (the emoji string).
      event.on(
        "emoji-click",
        emoji_click_decoder()
          |> dynamic_map(on_pick),
      ),
      // Close on outside click / Escape — handled by the popover/sheet
      // wrapper, not the picker itself. We only emit on_pick from here.
      attribute.attribute("data-on-close", "external"),
    ],
    [],
  )
}

// ---- helpers ----
// Decoder boilerplate. If the project already has a lustre/event.on
// pattern for custom-event decoding, reuse it instead.
import gleam/dynamic.{type Decoder, decode_field, string}

fn emoji_click_decoder() -> Decoder(String) {
  dynamic.field("detail", dynamic.field("unicode", string))
}

fn dynamic_map(d: Decoder(a), f: fn(a) -> b) -> Decoder(b) {
  fn(value) {
    case d(value) {
      Ok(v) -> Ok(f(v))
      Error(e) -> Error(e)
    }
  }
}
```

- [ ] **Step 2: Add the loader FFI**

In `web/src/sunset_web/sunset.gleam`:

```gleam
/// Lazily registers the `emoji-picker-element` web component. Idempotent;
/// safe to call on every picker open. Resolves the dynamic import on
/// first call, caches the promise on subsequent calls.
@external(javascript, "./sunset.ffi.mjs", "registerEmojiPicker")
pub fn register_emoji_picker() -> Nil
```

In `web/src/sunset_web/sunset.ffi.mjs`:

```javascript
let emojiPickerLoaded = null;
export function registerEmojiPicker() {
  if (!emojiPickerLoaded) {
    emojiPickerLoaded = import("emoji-picker-element");
  }
  return emojiPickerLoaded;
}
```

- [ ] **Step 3: Verify build**

```
nix develop --command bash -c "cd web && gleam build && npm run build --prefix . || true"
```

Expected: Gleam compiles; the npm step (production bundle) optionally validates the dynamic import resolves.

- [ ] **Step 4: Commit**

```bash
git add web/src/sunset_web/views/emoji_picker.gleam web/src/sunset_web/sunset.gleam web/src/sunset_web/sunset.ffi.mjs
git commit -m "$(cat <<'EOF'
web: add emoji_picker view + lazy-load FFI

emoji_picker.view wraps the <emoji-picker> web component as a Lustre
element with an on-pick callback decoded from the emoji-click
CustomEvent. registerEmojiPicker dynamic-imports the npm package on
first picker open, caching the promise.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task E2: "+" button + picker mounting

**Files:**
- Modify: `web/src/sunset_web/views/main_panel.gleam`
- Modify: `web/src/sunset_web.gleam`

- [ ] **Step 1: Add a "more" button at the end of the quick row**

In `crates/.../views/main_panel.gleam` — sorry, `web/src/sunset_web/views/main_panel.gleam` — find the `reaction_picker` function (the quick-row), and after the `list.map(quick_reactions, ...)` block, append a "more" button to the same flex row:

```gleam
let plus_button =
  html.button(
    [
      attribute.title("More reactions"),
      attribute.attribute("data-testid", "reaction-picker-more"),
      event.on_click(on_open_full_picker(msg_id)),
      ui.css([
        #("width", "32px"),
        #("height", "32px"),
        #("display", "inline-flex"),
        #("align-items", "center"),
        #("justify-content", "center"),
        #("padding", "0"),
        #("border", "none"),
        #("background", "transparent"),
        #("border-radius", "999px"),
        #("font-size", "18px"),
        #("color", p.text_muted),
        #("cursor", "pointer"),
      ]),
      [html.text("+")],
    )
```

(Append the `plus_button` to the end of the list passed as the second argument to `html.div(... attrs, items)`. Add `on_open_full_picker: fn(String) -> msg` as a parameter to `reaction_picker` and pass it through from `message_view` and `main_panel.view`.)

- [ ] **Step 2: Add `OpenFullEmojiPicker` Msg + Model field**

In `web/src/sunset_web.gleam`:

- Model: add `full_picker_for: option.Option(String),` (target id whose full picker is open).
- Msg: add `OpenFullEmojiPicker(String)` and `CloseFullEmojiPicker`.
- Update branches:

```gleam
OpenFullEmojiPicker(target) -> {
  // Trigger the lazy import so the web component is registered by the
  // time we mount it.
  sunset.register_emoji_picker()
  #(
    Model(..model, full_picker_for: option.Some(target), reacting_to: None),
    effect.none(),
  )
}
CloseFullEmojiPicker -> #(
  Model(..model, full_picker_for: option.None),
  effect.none(),
)
```

- [ ] **Step 3: Render the picker in `room_view`**

Add to `room_view` (next to where `reaction_sheet_el` is built):

```gleam
let full_picker_el = case model.full_picker_for {
  option.Some(target) ->
    case model.viewport {
      domain.Phone ->
        bottom_sheet.view(
          palette: palette,
          open: True,
          on_close: CloseFullEmojiPicker,
          test_id: "full-emoji-picker-sheet",
          content: emoji_picker.view(
            fn(emoji) { ToggleReactionEmoji(target, emoji) },
            CloseFullEmojiPicker,
          ),
        )
      domain.Desktop ->
        // Anchored popover. For simplicity, render as a fixed-position
        // overlay centered on the viewport for v1; refine to anchored
        // positioning in a follow-up.
        html.div(
          [
            ui.css([
              #("position", "fixed"),
              #("top", "50%"),
              #("left", "50%"),
              #("transform", "translate(-50%, -50%)"),
              #("z-index", "100"),
              #("background", palette.surface),
              #("border", "1px solid " <> palette.border),
              #("border-radius", "8px"),
              #("box-shadow", palette.shadow_lg),
            ]),
          ],
          [
            emoji_picker.view(
              fn(emoji) { ToggleReactionEmoji(target, emoji) },
              CloseFullEmojiPicker,
            ),
          ],
        )
    }
  option.None -> element.fragment([])
}
```

Plumb `full_picker_el` into the existing render tree (alongside `reaction_sheet_el`).

- [ ] **Step 4: Wire `on_open_full_picker` through main_panel**

`main_panel.view` accepts a new parameter `on_open_full_picker: fn(String) -> msg`. The dispatcher in `sunset_web.gleam`'s `room_view` passes `OpenFullEmojiPicker`. Cascade through `message_view` and `reaction_picker`.

- [ ] **Step 5: Verify build**

```
nix develop --command bash -c "cd web && gleam build"
```

Expected: clean build.

- [ ] **Step 6: Commit**

```bash
git add web/src/sunset_web/views/main_panel.gleam web/src/sunset_web.gleam
git commit -m "$(cat <<'EOF'
web: add "+" button + full emoji picker mount

Quick-row gets a trailing "+" that opens the lazy-loaded
emoji-picker-element. Mobile renders as a bottom sheet (reusing the
existing pattern); desktop uses a centered fixed-position overlay
for v1 — anchor refinement is a follow-up.

Picks dispatch ToggleReactionEmoji(target, emoji), reusing the same
toggle logic as the quick row.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Phase F — verification

### Task F1: Manual smoke test in two tabs

**Files:** none (verification only).

- [ ] **Step 1: Start the dev stack**

```
nix develop --command bash -c "cd web && npm run dev"
```

In a second terminal, start the relay if your existing dev workflow needs one (per the existing CLAUDE.md / dev docs). Adjust the URL the FE points to per your setup.

- [ ] **Step 2: Open two browser tabs in the same room with different identities**

(Easiest: regular tab + private/incognito tab. Identities are persisted to localStorage; different storage origins → different identities.)

- [ ] **Step 3: Run through the spec's Frontend test scenarios manually**

For each, verify the behavior matches:

- Alice taps 👍 on Bob's message → chip appears in both tabs with count 1, Alice's tab shows it filled, Bob's tab shows it outlined.
- Alice taps her own 👍 chip → chip disappears in both tabs.
- Alice opens the full picker via "+", picks 🦊 → chip appears with 🦊 × 1.
- Alice reacts to her own message → chip renders with by_you styling.
- On a mobile viewport (devtools narrow), the "+" picker opens as a bottom sheet.

- [ ] **Step 4: Run the full Rust test suite**

```
nix develop --command cargo test --workspace --all-features
```

Expected: all pass.

- [ ] **Step 5: Run clippy across the workspace**

```
nix develop --command cargo clippy --workspace --all-features --all-targets -- -D warnings
```

Expected: no warnings.

- [ ] **Step 6: Run the existing Playwright suite**

```
nix develop --command bash -c "cd web && npm run test:e2e"
```

Expected: all existing tests pass; reactions interactions don't regress receipts or message flows.

- [ ] **Step 7: Add a Playwright test for reactions**

Create or add to the appropriate file under `web/test/playwright/` (mirror the receipts test's structure). One scenario at minimum:

```typescript
test("reactions: alice reacts then bob sees the chip", async ({ context }) => {
  // Two pages = two identities (separate localStorage origins).
  const alice = await context.newPage();
  const bob = await context.newPage();
  await alice.goto(ROOM_URL);
  await bob.goto(ROOM_URL);
  await alice.getByPlaceholder(/message/i).fill("hello");
  await alice.getByPlaceholder(/message/i).press("Enter");
  // Bob waits for Alice's message and reacts.
  const aliceMessage = bob.getByText("hello");
  await aliceMessage.hover();
  await bob.getByLabel("React").click();
  await bob.getByTitle("React with 👍").click();
  await expect(alice.getByText("👍").first()).toBeVisible();
  await expect(bob.getByText("👍").first()).toBeVisible();
});
```

Run it:

```
nix develop --command bash -c "cd web && npm run test:e2e -- reactions"
```

Expected: PASS.

- [ ] **Step 8: Commit the Playwright test**

```bash
git add web/test/playwright/reactions.spec.ts
git commit -m "$(cat <<'EOF'
web: e2e test for reactions add → cross-tab visibility

Mirrors the receipts test pattern. Drives Alice→Bob via two browser
contexts; confirms Bob's reaction appears on Alice's tab.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Self-review

**Spec coverage check:**

| Spec section | Covered by |
|---|---|
| `MessageBody::Reaction` + `ReactionAction` wire format | Task A1 |
| Wire-format pin tests (Reaction Add + Remove) | Task A1 |
| `compose_reaction` helper + 64-byte cap | Task A2, A3 |
| Decode-side oversize emoji rejection | Task A4 |
| `Error::EmojiTooLong` | Task A2 |
| Re-exports from `sunset-core` | Task A5, B5 |
| `ReactionEvent`, `ReactionSnapshot`, `ReactionSig`, `ReactionHandles` | Task B1 |
| `apply_event` (LWW), `derive_snapshot` | Task B2 |
| `reactions_signature` (debounce) | Task B3 |
| `spawn_reaction_tracker` | Task B4 |
| Tracker integration test against `MemoryStore` | Task B4 |
| `Client::on_reactions_changed` | Task C1, C3 |
| `Client::send_reaction` | Task C4 |
| Tracker spawn in `Client::new` | Task C1 |
| JS marshaling (snapshot → JS Map) | Task C2 |
| Two-peer reaction round-trip (Add → Remove) | Task C5 |
| `emoji-picker-element` dependency + flake | Task D1 |
| Gleam FFI for `on_reactions_changed`, `send_reaction` | Task D2, D3 |
| Model.reactions snapshot dict shape | Task D4 |
| `ReactionsChanged` Msg + update | Task D6 |
| `ToggleReactionEmoji` (replaces `AddReaction`) — bridge-driven add/remove | Task D6 |
| View-time snapshot → `List(Reaction)` | Task D5 |
| `on_reactions_changed` wiring at client-ready | Task D7 |
| "+" button + picker mounting | Task E1, E2 |
| `emoji-picker-element` lazy loading + Lustre wrapping | Task E1 |
| Mobile bottom-sheet for picker | Task E2 |
| Manual UX smoke + workspace tests + Playwright | Task F1 |

No spec section is uncovered.

**Placeholder scan:** No "TBD"/"TODO"/vague-instruction tokens. Each step shows the exact code or command. The two follow-ups left explicit are (a) anchored desktop popover positioning for the full picker, and (b) memoizing the self-pubkey hex if profiling flags it — both are flagged as deliberate future polish, not gaps.

**Type consistency:** `compose_reaction` signature matches across A3, C4, B4, C5. `ReactionHandles` field names (`on_reactions_changed`, `last_target_signatures`) match in B1, B4, C1, C3. `IncomingReactionsSnapshot` accessor names (`reactions_snapshot_target_hex`, `reactions_snapshot_entries`) match between Gleam externals (D2) and JS impls (D3) and FE consumer (D7). `ToggleReactionEmoji(target, emoji)` is the post-rename payload everywhere it's referenced (D6 onwards).
