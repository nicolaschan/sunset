# Channels within rooms — design

- **Date:** 2026-05-04
- **Status:** Draft
- **Scope:** Add a `channel` label to chat messages (and reactions/receipts) so a single sunset.chat room can host multiple text channels (#general, #links, #off-topic, …). The label rides inside the AEAD plaintext and is covered by the inner Ed25519 signature, so the relay sees `<room_fp>/msg/<hash>` as before — never the channel.

## Problem

A sunset room today is one undifferentiated stream: every Text in the room shows up in one place. The Lustre UI ships a hardcoded fixture for the channels rail (`fixture.channels()` — "general", "links", "build-log", "off-topic") plus a `current_channel` on the room model that nothing actually filters on. We want real channels: posts to `#links` should not appear in `#off-topic`, and switching channels in the UI should change what you read and what you reply to, without doing anything observable to relays beyond what they already see.

## Goals

- Add a per-message `channel` label that is part of the signed plaintext, not part of the store key.
- Default channel `"general"` so existing flows keep working with no caller changes at the call sites that don't yet know about channels.
- Reactions and receipts inherit the channel of the message they reference.
- Per-channel filtering happens after decode (in `OpenRoom` / `ReactionTracker`); the relay-visible namespace is unchanged (`<room_fp>/msg/<hash>`).
- Wire up the existing channels rail in the Gleam UI to drive off real, observed channels — replace the static fixture.

## Non-goals

- Voice channels: the existing single Lounge stays as-is. The voice signaling namespace is `<room_fp>/webrtc/`, separate from `/msg/`. A future plan can extend voice to multiple rooms-per-room; not this PR.
- Channel ACLs / per-channel admission. v1 has no admin/role surface to gate against.
- Per-channel notification settings, mute, archive, pinned messages.
- Channel topic / description / position. Channel labels are bare strings; ordering in the rail is a UI concern.
- Persistent channel registry. The channel set is implicit — derived from the union of channels seen in messages, plus the always-present default. No `MessageBody::ChannelInfo` event yet; we can add one later without protocol churn (it would just be a new variant).
- Cross-room channels.
- Migrating any existing on-disk store data. There's no production deployment; the wire format will bump cleanly with updated frozen vectors.

## Approach

### Cryptographic placement

The `channel` field lives at the `SignedMessage` level — inside the AEAD ciphertext, covered by the inner Ed25519 signature. Concretely:

```rust
// Was:
pub struct SignedMessage {
    pub inner_signature: Signature,
    pub sent_at_ms: u64,
    pub body: MessageBody,
}

// Becomes:
pub struct SignedMessage {
    pub inner_signature: Signature,
    pub sent_at_ms: u64,
    pub channel: ChannelLabel,
    pub body: MessageBody,
}
```

`InnerSigPayload` mirrors that shape so the inner signature commits to the channel:

```rust
pub struct InnerSigPayload<'a> {
    pub room_fingerprint: &'a [u8; 32],
    pub epoch_id: u64,
    pub sent_at_ms: u64,
    pub channel: &'a ChannelLabel,
    pub body: &'a MessageBody,
}
```

`MessageBody` itself is unchanged — Text / Receipt / Reaction shapes don't change, only the envelope they sit inside grows a sibling field. Putting the channel on the outer `SignedMessage` (rather than threading it into each variant) means: (a) reactions and receipts get their channel "for free", (b) we don't have to fan out the change across every variant of `MessageBody`, and (c) `MessageBody`'s frozen postcard hex pins (`message_body_text_postcard_hex_pin` etc.) stay valid — only the `SignedMessage`-level frozen vector and the `EncryptedMessage` size-shape change.

The store key (`<room_fp>/msg/<value_hash>`) does not include the channel. From the relay's point of view, two posts to two different channels look identical (same prefix, both opaque ciphertext). This is the privacy-preserving choice — it matches the project's existing position that the relay should learn as little as possible beyond room identity and timing.

### `ChannelLabel`

A newtype:

```rust
#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub struct ChannelLabel(String);
```

Validation (in a `try_new` constructor and on decode):

- Length 1..=64 bytes UTF-8.
- Must not contain `'\0'` or any ASCII control character (`< 0x20`). Practical reason: these would render badly and are never the user's intent. Allows everything else (Unicode, emoji, spaces).
- Must not be all-whitespace. Empty-after-trim → reject.

A constant for the default:

```rust
pub const DEFAULT_CHANNEL: &str = "general";
impl ChannelLabel {
    pub fn default_general() -> Self { Self("general".to_owned()) }
}
```

`Default` is **not** implemented (we want the choice to be explicit at the call site).

`decode_message` returns `Error::BadChannel` on a malformed label. Not a panic — a malformed label is a peer/protocol bug we want to surface, not a crash.

### Compose / decode API changes

`compose_message` grows a `channel: ChannelLabel` parameter, threaded into both `SignedMessage` and `InnerSigPayload`. The convenience wrappers grow it too:

```rust
pub fn compose_text(identity, room, epoch_id, sent_at_ms, channel: ChannelLabel, text, rng)
pub fn compose_receipt(identity, room, epoch_id, sent_at_ms, channel: ChannelLabel, for_value_hash, rng)
pub fn compose_reaction(identity, room, epoch_id, sent_at_ms, channel: ChannelLabel, payload, rng)
```

`DecodedMessage` exposes the channel:

```rust
pub struct DecodedMessage {
    pub author_key: IdentityKey,
    pub room_fingerprint: RoomFingerprint,
    pub epoch_id: u64,
    pub channel: ChannelLabel,        // NEW
    pub value_hash: Hash,
    pub sent_at_ms: u64,
    pub body: MessageBody,
}
```

### `OpenRoom` API changes

`send_text` and `send_reaction` grow a `channel: ChannelLabel` parameter. No defaulting magic — the host passes the active channel explicitly.

The decode loop's callbacks gain the channel:

```rust
pub fn on_message<F: Fn(&DecodedMessage, bool /* is_self */)>(&self, cb: F)
// (DecodedMessage already carries channel — no signature change other than that.)

pub fn on_receipt<F: Fn(Hash /* for_value_hash */, &IdentityKey, &ChannelLabel, u64 /* sent_at_ms */)>(&self, cb: F)
// Channel of the original Text the receipt acks. Comes from the receipt's own signed envelope.

pub fn on_reactions_changed<F: Fn(&Hash /* target */, &ChannelLabel, &ReactionSnapshot)>(&self, cb: F)
// Channel of the *target* message (per ReactionTracker bookkeeping).
```

Auto-ack inherits the target message's channel: when the decode loop sees a Text from another peer in channel `C`, it composes the Receipt with `channel = C`. The host receives both events naturally bound to the same channel.

### Reaction tracker

`ReactionTracker` (in `sunset-core::reactions`) currently subscribes to `<room_fp>/msg/`, decodes each entry, and folds reactions per `(target_value_hash, emoji, author)`. The change:

- Per-target bookkeeping additionally records `channel: ChannelLabel` (the channel of the *target* message, learned the first time a Text for that target is decoded).
- For Reaction events that arrive before the target Text (rare but possible during initial sync), we hold the snapshot indexed by target and stamp the channel when the Text arrives. If the target never arrives, the snapshot just doesn't get a channel — the on-change callback only fires once we have one. This is consistent with how reactions need a target message to render meaningfully.
- The on-change callback signature gains `channel: &ChannelLabel`; the host filters in its renderer.

### Discovery: where does the channel list come from?

Implicit. Each `OpenRoom` exposes:

```rust
pub fn observed_channels(&self) -> Vec<ChannelLabel>;
pub fn on_channels_changed<F: Fn(&[ChannelLabel])>(&self, cb: F);
```

Implementation: the decode loop maintains a `BTreeSet<ChannelLabel>` (sorted, dedup'd). The set always contains `ChannelLabel::default_general()`. Every successfully decoded message — Text, Receipt, or Reaction — contributes its channel; new channels fire `on_channels_changed` with the current sorted snapshot. Cheap — one `insert` + a borrow on the callback path.

Decode-loop triggering: today the loop is started lazily on the first `on_message` / `on_receipt` registration. We extend the trigger set to also start it on the first `on_channels_changed` registration so a host that wires channels up first (before the per-message callback) doesn't sit on a quiet stream.

This avoids defining a registry event in v1 and keeps the protocol surface flat. If a coordination/role/admin layer arrives later, an explicit `MessageBody::ChannelInfo` variant can supplement (not replace) the implicit set.

### WASM bridge changes (`sunset-core-wasm` / `sunset-web-wasm`)

Thin pass-throughs. `RoomHandle`:

```rust
pub async fn send_message(&self, channel: String, body: String, sent_at_ms: f64) -> Result<String, JsError>;
pub async fn send_reaction(&self, channel: String, target_hex: String, emoji: String, action: String) -> Result<(), JsError>;

pub fn on_message(callback: js_sys::Function);   // IncomingMessage now has `channel: String`
pub fn on_receipt(callback: js_sys::Function);   // IncomingReceipt now has `channel: String`
pub fn on_reactions_changed(callback: js_sys::Function);
                                                  // payload now has `channel: String`

pub fn on_channels_changed(callback: js_sys::Function);  // List<String>
pub fn observed_channels() -> Vec<JsValue>;              // for first-paint
```

`IncomingMessage`/`IncomingReceipt` grow a `channel` field. All the channel-string conversions happen in one helper (`channel_label_from_js(&str) -> Result<ChannelLabel, JsError>`); business logic — validation included — stays in `sunset-core::ChannelLabel::try_new`.

### Gleam UI changes

`domain.gleam`:

- `ChannelId(String)` already exists; we'll keep it as the UI-facing handle and add a `default_channel_id() = ChannelId("general")` helper. (We can use the same string the wasm side validates against; the Rust side is the source of truth for "is this a legal channel".)
- `Message` gets a `channel: String` field.
- The fixture `channels()` is dropped from the UI's source of truth — `RoomState` builds its channel list from observations.

`RoomState` adds:

```gleam
pub type RoomState {
  RoomState(
    handle: Option(RoomHandle),
    messages: List(domain.Message),                  // unfiltered
    members: List(domain.Member),
    receipts: Dict(String, Dict(String, Int)),
    reactions: Dict(String, List(Reaction)),
    current_channel: ChannelId,                      // unchanged
    channels: List(domain.Channel),                  // NEW: observed + default
    draft: String,
    selected_msg_id: Option(String),
    reacting_to: Option(String),
    sheet: Option(domain.Sheet),
    peer_status_popover: Option(domain.MemberId),
    revealed_spoilers: Set(#(String, String)),
  )
}
```

Render path: messages list is filtered by `state.current_channel` before being passed to `main_panel.view`. The composer's `SubmitDraft` sends with `channel = current_channel`. The channels rail iterates `state.channels`. `unread` per channel is best-effort (count of messages in that channel since last view) — out of scope here, leave it as 0 for v1 and add later.

A `ChannelsObserved(room_name, List(String))` Msg arrives from `on_channels_changed`. The reducer merges into `state.channels`, preserving rail order (alphabetical for now; default `"general"` always pinned to top).

### Testing

Layered:

- `sunset-core` unit tests: `compose_text`/`compose_reaction`/`compose_receipt` + `decode_message` round-trip with an explicit channel, including (a) cross-channel posts decode independently, (b) inner-signature forgery still rejected when channel is tampered with, (c) malformed channels (empty, all-whitespace, control chars, >64 bytes) round-trip-error correctly, (d) wire-format frozen vector for `SignedMessage` + `EncryptedMessage` updated and asserted.
- `sunset-core` peer-level test: `OpenRoom::send_text("alpha", ...)` followed by another peer subscribing — `on_message` fires with `decoded.channel == "alpha"`, and `on_channels_changed` fires with `["general", "alpha"]`. The reaction tracker callback's channel matches the target Text's channel.
- `sunset-core-wasm`: smoke that the channel string survives the JS round-trip.
- Playwright e2e: two browsers join the same room, switch to a non-default channel `"links"`, post and read; verify the message does NOT appear in `#general` for the other browser, and DOES appear in `#links`. Cross-check that unread/read indicator behavior keeps working in `#general`.

## Observable behavior after this PR

- A user sees the channels rail derive from real channels in the room. `#general` is always present. Posting to `#general` and `#links` from one browser leads to two distinct lists in the other browser, switchable via the rail.
- Existing rooms (which had no channel field) will still decode if read back from a memory store that pre-dates this change — but: there's no persistent backend deployed, and the in-memory store is rebuilt every page load, so this is purely a wire-format bump for actively-syncing peers. We document the bump in the spec; no migration code.
- Relay logs and store layouts are unchanged: same `<room_fp>/msg/<hash>` keys, same opaque ciphertext.

## Risks and trade-offs

- **Wire-format bump.** Affects every running peer at the moment they update. Acceptable: pre-prod, in-memory persistence, no users. Documented in the spec by updating the frozen vectors.
- **Channel labels are arbitrary strings.** Two users can talk past each other if one types `"links"` and the other `"Links"`. Trade-off: case-sensitive matches Discord/Slack-ish "channel name = a string"; normalization (lowercasing, trimming) happens in the UI before send if at all. Don't normalize in `sunset-core` — preserves ground truth. Could revisit after user feedback.
- **No registry.** A channel only becomes visible to peers once a message is posted to it. UX-wise: typing a new name in the composer's "channel" indicator should immediately add it to the rail locally; once you post, others see it too. We allow the local rail to include channels that have no messages yet (so the user can compose into a fresh channel).
- **Channel set grows monotonically.** No way to delete a channel beyond GC of its messages. Acceptable for v1.
- **Wire format pinning.** The existing `encrypted_message_frozen_vector` test uses a hardcoded ciphertext, so it isn't affected by the inner-plaintext change. The existing `MessageBody`-level pins (`message_body_text_postcard_hex_pin`, etc.) are also unaffected — `MessageBody` itself didn't move. To lock in the new `SignedMessage` shape, we add a new `signed_message_frozen_vector` test asserting a known hex of `postcard::to_stdvec(&SignedMessage { … channel: "general", … })` so accidental drift breaks the build.

## Open questions

- Should channel matching be case-insensitive in the protocol? Recommendation: no; the application layer can normalize. Keep `sunset-core` ground-truth.
- Should the implicit channel set persist across sessions (e.g. via an out-of-band local cache so the rail isn't empty until messages re-sync on cold start)? Not for v1: with replay-on-subscribe the set rebuilds in the first round-trip. Revisit if it feels janky in practice.
