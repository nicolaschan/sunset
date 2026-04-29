# Delivery receipts — design

**Status:** draft
**Date:** 2026-04-29
**Surface:** `crates/sunset-core` (wire format) · `crates/sunset-web-wasm` (auto-ack) · `web/` (FE pending → delivered UI)

## Goal

When the user sends a chat message, render it as visually pending (gray
or otherwise muted) until at least one other peer in the room has
acknowledged it. Once any receipt arrives, the message flips to its
solid "delivered" state.

The mechanism: receipts ride the existing chat-message wire path —
same room namespace, same authorship signature, same AEAD envelope.
The only thing distinguishing a receipt from a text message is the
body's enum variant. Auto-acknowledgement is handled at the
sunset-web-wasm bridge layer: when the bridge decodes a non-self
`Text` message, it composes and inserts a `Receipt` referencing that
message's value-hash. The FE never has to think about it; it just
gets a `on_receipt(value_hash, from_pubkey)` callback alongside its
existing `on_message`.

## Non-goals

- Read receipts (vs. delivery). This spec only confirms a peer's
  client received and decoded the message — not that the user
  visually saw it.
- Multi-device dedupe. If a user has two clients open they'll send
  two receipts for the same message; the FE simply collects them in a
  set keyed by `(message_id, peer_verifying_key)`.
- Per-recipient delivery accounting beyond the binary "any" count.
  The first receipt is enough to flip the UI; the
  `MessageDetails.receipts` panel can show the full list, but UI for
  that already exists from the D1 design and just needs data.
- TTL / expiration. Receipts are durable like the messages they
  reference; ~64 bytes per receipt is acceptable. (Future
  optimisation: move receipts to the ephemeral bus once we're
  confident other peers don't need to replay them — but that's out of
  scope here.)
- Migration of existing messages. Pre-receipt messages stay text-only
  in their inner signature payload; the wire-format change is not
  backwards-compatible. We accept the break (pre-1.0 software, no
  external clients yet).

## Decisions

| Decision | Choice |
|---|---|
| Receipt namespace | Same `<room_fp>/msg/<value_hash>` as chat messages |
| Receipt encryption | Same AEAD envelope; receipts encrypted same as text |
| Receipt authorship | Signed by the receiver's identity (same outer + inner sig as text) |
| Loop avoidance | Receipts never trigger auto-receipts; only `Text` does |
| Self-receipts | Skipped — a peer doesn't ack its own message |
| TTL | None; receipts are durable |
| FE state shape | `Dict(message_id, Set(VerifyingKey))` |
| "Delivered" threshold | ≥1 receipt from a peer other than self |

## Wire format

### `MessageBody` enum

`crates/sunset-core/src/crypto/envelope.rs`:

```rust
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MessageBody {
    Text(String),
    Receipt { for_value_hash: Hash },
}
```

Where `Hash` is the existing 32-byte hash type from `sunset-store`.

### `SignedMessage` change

The inner-AEAD plaintext changes from `body: String` to
`body: MessageBody`:

```rust
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedMessage {
    pub inner_signature: Signature,
    pub sent_at_ms: u64,
    pub body: MessageBody,
}
```

### `InnerSigPayload` change

The inner Ed25519 signature must cover the body. The current payload
has `body: &'a str`; it changes to `body: &'a MessageBody`:

```rust
#[derive(Serialize)]
pub struct InnerSigPayload<'a> {
    pub room_fingerprint: &'a [u8; 32],
    pub epoch_id: u64,
    pub sent_at_ms: u64,
    pub body: &'a MessageBody,
}
```

`inner_sig_payload_bytes(...)` is updated to take a `&MessageBody`.

### `EncryptedMessage` and `message_name`

Unchanged. The outer KV entry still lands at
`<room_fp>/msg/<value_hash>`. A receipt's value_hash is its own
`ContentBlock.hash()` (i.e., the receipt has its own unique
location), and the body it references is in `MessageBody::Receipt {
for_value_hash }`.

### Wire-format pin

`crates/sunset-core/src/crypto/envelope.rs` has a hex-pinned test
vector for the canonical signing payload. Update the vector to cover
the new `MessageBody` enum encoding (a `Text` and a `Receipt`
exemplar) so accidental drift breaks the build.

## Compose / decode API

### `compose_message`

Signature changes from `body: &str` to `body: MessageBody`:

```rust
pub fn compose_message<R: CryptoRngCore + ?Sized>(
    identity: &Identity,
    room: &Room,
    epoch_id: u64,
    sent_at_ms: u64,
    body: MessageBody,
    rng: &mut R,
) -> Result<ComposedMessage>
```

Two thin convenience helpers wrap it:

```rust
pub fn compose_text<R>(identity, room, epoch_id, sent_at_ms, text: &str, rng) -> Result<ComposedMessage>
pub fn compose_receipt<R>(identity, room, epoch_id, sent_at_ms, for_value_hash: Hash, rng) -> Result<ComposedMessage>
```

Both delegate to `compose_message` with the right body variant. This
keeps callers from needing to import `MessageBody` directly for the
common cases.

### `decode_message`

`DecodedMessage` body type changes:

```rust
pub struct DecodedMessage {
    pub author_key: IdentityKey,
    pub room_fingerprint: RoomFingerprint,
    pub epoch_id: u64,
    pub value_hash: Hash,
    pub sent_at_ms: u64,
    pub body: MessageBody,
}
```

The decode path is otherwise unchanged: AEAD-decrypt, postcard-parse
`SignedMessage`, verify inner signature over `MessageBody`.

## sunset-web-wasm bridge

### Auto-acknowledge in `spawn_message_subscription`

The existing subscription that decodes incoming messages becomes
variant-aware:

```rust
let decoded = decode_message(&room, &entry, &block)?;
let is_self = decoded.author_key == identity_pub;

match decoded.body {
    MessageBody::Text(text) => {
        // Hand off to the FE on_message callback as today.
        deliver_to_on_message(decoded, text, is_self);

        // Auto-ack: only for non-self texts.
        if !is_self {
            send_receipt(&store, &room, &identity, decoded.value_hash, &mut rng).await;
        }
    }
    MessageBody::Receipt { for_value_hash } => {
        // Don't deliver to on_message. Drop self-Receipts at the
        // bridge — auto-ack never produces them (the self gate above
        // skips), so a self-Receipt could only come from manual
        // composition (e.g. tests). Filtering at the source means
        // the FE doesn't need a redundant `is_self` check.
        if !is_self {
            deliver_to_on_receipt(for_value_hash, decoded.author_key);
        }
    }
}
```

`send_receipt` composes a `MessageBody::Receipt` entry (signed by
`identity`, AEAD'd to the room) and inserts it into the local store.
The store's normal sync path fans it out to peers; the original
sender's bridge will then receive it and fire its own
`on_receipt` callback.

### New JS-facing API

`crates/sunset-web-wasm/src/client.rs`:

```rust
pub fn on_receipt(&self, callback: js_sys::Function) {
    *self.on_receipt.borrow_mut() = Some(callback);
    // No new subscription needed — spawn_message_subscription handles
    // both variants. on_receipt is just registered.
}
```

The JS shape passed to the callback:

```ts
type IncomingReceipt = {
    for_value_hash_hex: string;
    from_pubkey: BitArray;
};
```

Mirror of the existing `IncomingMessage` shape minus author-side
metadata (the bridge already drops self-Receipts, so callers can
treat every fired receipt as "from a peer").

`sunset-web-wasm/src/messages.rs` adds an `IncomingReceipt` JS object
constructor, parallel to the existing `IncomingMessage`.

### `send_message` is unchanged at the JS API level

`Client::send_message(body: String, sent_at_ms, nonce_seed)` keeps
its signature; it now wraps `body` in `MessageBody::Text` internally.

## Frontend

### Domain + model

`web/src/sunset_web/domain.gleam`:

```gleam
/// A peer's verifying key, opaque hex-prefix identifier from the
/// pubkey bytes. Stored as String so Dict lookup is cheap.
pub type ReceiptFrom = String
```

`web/src/sunset_web.gleam` Model:

```gleam
/// Receipts received per outgoing message, keyed by message id
/// (value_hash hex). Each entry is the set of peer verifying keys
/// that have acknowledged. Self-receipts are filtered out at insert
/// time (we never count our own ack toward "delivered").
receipts: Dict(String, set.Set(ReceiptFrom)),
```

### New Msg variant

```gleam
/// from_pubkey is the receiver's pubkey, exposed as the same hex
/// format the rest of the app uses for short identifiers. The bridge
/// only fires this Msg for receipts authored by other peers; self-
/// receipts (which auto-ack never produces) are dropped at the
/// bridge layer, so this branch can unconditionally insert.
IncomingReceipt(message_id: String, from_pubkey: String)
```

Update branch:

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

### FFI subscription

`web/src/sunset_web/sunset.gleam` and `sunset.ffi.mjs`: a new
`on_receipt(client, callback)` external mirroring `on_message`.

In `init`'s effect batch (or wherever `on_message` is wired today —
currently in `ClientReady`), add:

```gleam
sunset.on_receipt(client, fn(r) {
  dispatch(IncomingReceipt(
    sunset.receipt_value_hash_hex(r),
    sunset.receipt_from_pubkey_short(r),
  ))
})
```

### Rendering

`web/src/sunset_web/views/main_panel.gleam` `message_view` branches
on whether the message is "pending":

```gleam
let pending = m.you && {
  case dict.get(receipts, m.id) {
    Ok(s) -> set.size(s) == 0
    Error(_) -> True
  }
}
```

Pending messages render with reduced opacity (e.g. `opacity: 0.55`)
on the bubble background and text. The existing `pending: Bool`
field on `domain.Message` is repurposed — it currently flickers true
during the brief send window; now it's wholly receipt-driven.

A short fade transition (`transition: opacity 220ms ease`) makes the
flip feel intentional rather than a flash.

The `MessageDetails` side panel's `receipts` list (from the D1
design) gets populated from the same `model.receipts` dict, mapped
through the room's known members to surface friendly names.

## Loop avoidance

Three rules, enforced in the wasm bridge:

1. **Variant gate.** Only `MessageBody::Text` triggers a receipt.
   Receipts never trigger receipts.
2. **Self gate.** A peer never auto-acks its own outgoing message.
   The bridge checks `decoded.author_key == identity_pub` before
   calling `send_receipt`.
3. **Replay safety.** `Replay::All` redelivers historical entries on
   client startup. The bridge would re-auto-ack every previously-seen
   text. To avoid storing duplicate receipts, the bridge consults
   the local store before writing a receipt: if a receipt with our
   verifying key already exists referencing `for_value_hash`, skip
   the insert. Implementation: a single-key store lookup keyed on a
   deterministic name derived from `(self_vk, for_value_hash)`. See
   "Receipt naming" below.

### Receipt naming

A receipt's `value_hash` is content-addressed (its own
ContentBlock), but the deduplication rule needs a way to find an
*existing* receipt without a full scan. We don't add a new
namespace; instead, before composing, the bridge does:

```rust
// Walk store entries under <room_fp>/msg/, decode each as a
// MessageBody, skip if any Receipt with for_value_hash == target
// from author == self_vk.
```

For v1 this scan is acceptable — the room's history is bounded by
the user's local view. Once history grows, a proper index can be
added (out of scope).

## Testing

### Core (`sunset-core`)

- Unit: `compose_text` round-trips through `decode_message` and
  matches the previous behaviour for plaintext bodies (regression
  test that the enum tag overhead doesn't reorder the existing
  fields).
- Unit: `compose_receipt` round-trips; the decoded body is
  `Receipt { for_value_hash }` with the expected hash.
- Unit: a receipt signed by Alice cannot be fraudulently decoded as
  one signed by Bob (signature verification still gates).
- Wire-format pin: hex test vectors for one Text and one Receipt.

### Bridge (`sunset-web-wasm`)

- Integration: two-engine test where Alice sends a Text, Bob's
  bridge auto-emits a Receipt, Alice's bridge fires `on_receipt`
  with Bob's pubkey and the original value_hash.
- Replay safety: re-running Bob's `spawn_message_subscription`
  against the same store doesn't produce duplicate receipts.
- Loop avoidance: when Alice receives Bob's Receipt, no further
  receipt is written (bridge does not auto-ack a Receipt variant).

### Frontend (Playwright)

- Two browser tabs in the same room; Alice sends; Alice's bubble
  starts at reduced opacity, then transitions to full opacity once
  Bob's tab is up and the receipt arrives.
- Self-receipts don't flip the UI — Alice opens a second tab as
  herself, sends, the bubble stays pending until a third (different
  identity) tab opens and acks.
- Receipts visible in the message-details side panel (existing UI),
  populated with at least one receiver name.

## Out-of-scope follow-ups

- **Read receipts.** Distinguish "decoded by client" (this spec)
  from "rendered to user" (visibility tracking).
- **Per-peer "delivered" indicators** (Slack-style "delivered to N
  of M peers"). The data is already there in
  `Dict(message_id, Set(...))`; only UI work.
- **Receipts via the ephemeral bus.** Once we've validated that
  receipts don't need durable replay (e.g., a client coming online
  weeks later doesn't care about old delivery state), move receipts
  off the durable store and onto `BusImpl::publish_ephemeral` to
  avoid the store-growth cost.
- **Receipt index.** A small `(receiver_vk, for_value_hash) → bool`
  KV index would replace the linear scan in the dedup check and the
  side-panel population.

## Open questions

None at design time. Implementation may surface decisions around the
exact opacity value, the transition curve, and whether to show a
small "✓" tick mark instead of (or in addition to) the opacity
change — those are UI polish, fine to resolve during implementation.
