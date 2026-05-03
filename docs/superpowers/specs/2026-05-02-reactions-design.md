# Reactions — design

**Status:** draft
**Date:** 2026-05-02
**Surface:** `crates/sunset-core` (wire format, compose helpers, `ReactionTracker`) · `crates/sunset-web-wasm` (callback wiring) · `web/` (state + UI + picker)

## Goal

A user can attach unicode emoji reactions to any chat message in the
room, including their own. Each user can attach multiple distinct emoji
to the same message, and tapping an emoji they've already attached
removes it (toggle). Reactions ride the existing chat-message wire
path: same room namespace, same AEAD envelope, same authorship
signature — only the `MessageBody` enum variant distinguishes them.

The fold from the event stream into "current reactions per message
per emoji" lives in `sunset-core` as a self-driven `ReactionTracker`,
mirroring the existing `MembershipTracker` (lifted out of
`sunset-web-wasm` in commit `0864efa`). The wasm bridge does not
pump events into the tracker; it spawns it once and registers a
callback. The same tracker drives wasm (web client), TUI, and any
future surface (Minecraft mod, native relay) without duplicating fold
logic.

## Non-goals

- **Receipts cleanup.** Receipts currently fold in the FE. Lifting
  them into a parallel `ReceiptTracker` is a mechanical port of the
  same pattern but is its own follow-up.
- **Unified `MessageDispatcher`.** The reaction tracker, message
  subscription, and (eventual) receipt tracker each subscribe
  independently to `<room_fp>/msg/*`. A single subscription that
  fans out by variant is a future cleanup once the duplication
  actually matters.
- **Reaction notifications.** No "Bob reacted to your message"
  toast / sound / unread-marker. Chips are silent.
- **Custom / room emoji.** Slack-style `:room-emoji:` uploads
  require content-addressed blob refs and an upload UX; entirely
  separate spec.
- **Rate limiting / spam guards.** Future moderation layer.
- **Migration of historical messages.** Pre-reaction messages decode
  unchanged because we add (not reorder) `MessageBody` variants;
  no migration needed.

## Decisions

| Decision | Choice |
|---|---|
| Emoji representation | Free-form unicode `String`, length-capped at 64 bytes on compose |
| Multiple per user per message | Yes |
| Removal | Yes; modeled as `Reaction { action: Remove }` event, not store-level mutation |
| Self-reactions | Allowed |
| Conflict resolution | LWW on `(author, target_message, emoji)` by `sent_at_ms`; tiebreak by `value_hash` lex order |
| Storage namespace | Same `<room_fp>/msg/<value_hash>` as text + receipts |
| Encryption + signing | Same outer AEAD + inner Ed25519 signature; reactions are first-class signed messages |
| Loop avoidance | Trivial — reactions are user-driven only; no auto-emit anywhere |
| Replay safety | Tracker fold is idempotent (LWW), so `Replay::All` redelivery converges to the same state |
| Picker | `emoji-picker-element` web component (~30 KB) for the full picker; quick-row of 6 common emoji rendered by us |
| UI shape | Discord-style: chip row below the bubble, tap chip to toggle your own reaction, "+" at end opens picker |
| Where the fold lives | `sunset-core` (`ReactionTracker`), self-driven over its own `<room_fp>/msg/` subscription |

## Wire format

### `MessageBody` enum

`crates/sunset-core/src/crypto/envelope.rs`:

```rust
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MessageBody {
    Text(String),
    Receipt { for_value_hash: Hash },
    Reaction {
        for_value_hash: Hash,
        emoji: String,
        action: ReactionAction,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReactionAction { Add, Remove }
```

Postcard-compatible: variants are *added*, not reordered, so existing
`Text`/`Receipt` entries decode unchanged.

### Signing payload, envelope, name

`InnerSigPayload` already takes `&MessageBody` and so its
hex-pinned signing-vector test simply gains additional exemplars
covering `Reaction { Add }` and `Reaction { Remove }`. `SignedMessage`,
`EncryptedMessage`, and the outer KV name (`<room_fp>/msg/<value_hash>`,
where `value_hash` is the reaction entry's own `ContentBlock.hash()`)
are unchanged.

### Wire-format pin

`crypto/envelope.rs` adds two new hex test vectors covering one
`Reaction::Add` and one `Reaction::Remove`. Drift breaks the build.

## Compose / decode API

### `compose_reaction`

`crates/sunset-core/src/message.rs`:

```rust
pub fn compose_reaction<R: CryptoRngCore + ?Sized>(
    identity: &Identity,
    room: &Room,
    epoch_id: u64,
    sent_at_ms: u64,
    for_value_hash: Hash,
    emoji: &str,
    action: ReactionAction,
    rng: &mut R,
) -> Result<ComposedMessage>
```

Validates `emoji.len() <= 64` and returns a new
`Error::EmojiTooLong { len: usize }` otherwise. (64 bytes covers all
unicode emoji including ZWJ family sequences.) No content-shape
validation — that's the picker's job; the wire format is permissive.

### `decode_message`

No struct change. The new `Reaction` variant simply appears in
`DecodedMessage.body`, and all existing exhaustive matches on
`MessageBody` get a compiler-enforced new arm.

Decode also enforces the 64-byte cap defensively, returning the same
`Error::EmojiTooLong { len }` so a peer cannot craft an oversized-emoji
entry that bypasses our compose validation.

## `ReactionTracker` in `sunset-core`

`crates/sunset-core/src/reactions.rs` — pattern matches
`crates/sunset-core/src/membership.rs`.

### Types

```rust
pub type ReactionSnapshot = HashMap<String, BTreeSet<IdentityKey>>;
//                                  emoji   authors

#[derive(Clone, Debug)]
struct ReactionEntry {
    action: ReactionAction,
    sent_at_ms: u64,
    value_hash: Hash, // tiebreak in LWW
}

#[derive(Clone, Debug)]
pub struct ReactionEvent {
    pub author: IdentityKey,
    pub target: Hash,
    pub emoji: String,
    pub action: ReactionAction,
    pub sent_at_ms: u64,
    pub value_hash: Hash,
}

pub type ReactionsCallback = Box<dyn Fn(&Hash, &ReactionSnapshot)>;
pub type ReactionsCallbackSlot = Rc<RefCell<Option<ReactionsCallback>>>;

#[derive(Clone, Default)]
pub struct ReactionHandles {
    pub on_reactions_changed: ReactionsCallbackSlot,
    /// Per-target last-fired snapshot signature. Allows re-registering
    /// the callback to fire a fresh snapshot on demand (mirrors
    /// `TrackerHandles::last_signature`).
    pub last_target_signatures: Rc<RefCell<HashMap<Hash, ReactionSig>>>,
}
```

`ReactionSig` is a stable derived shape (sorted `Vec<(emoji, Vec<author>)>`)
used to debounce callback fires.

### Pure helpers (testable in isolation)

```rust
/// Apply one event to in-memory state. Returns `true` if the snapshot
/// for `event.target` may have changed (signature comparison happens
/// in the caller).
pub fn apply_event(
    state: &mut HashMap<Hash, HashMap<String, HashMap<IdentityKey, ReactionEntry>>>,
    event: ReactionEvent,
) -> bool;

/// Pure derivation: render the current snapshot for one target.
pub fn derive_snapshot(
    state: &HashMap<Hash, HashMap<String, HashMap<IdentityKey, ReactionEntry>>>,
    target: &Hash,
) -> ReactionSnapshot;

/// Stable signature for debounce.
pub fn reactions_signature(snapshot: &ReactionSnapshot) -> ReactionSig;
```

LWW rule in `apply_event`: for `(author, target, emoji)` keep the
entry with the highest `(sent_at_ms, value_hash)` lex pair. The
`Add`/`Remove` action is the value of that winning entry; if the
winner is `Remove`, the author is omitted from the snapshot.

### Spawn entrypoint

```rust
pub fn spawn_reaction_tracker<S: Store + 'static>(
    store: std::sync::Arc<S>,
    room: Room,
    room_fp_hex: String,
    handles: ReactionHandles,
);
```

The spawned task:

1. `store.subscribe(Filter::NamePrefix(<room_fp>/msg/), Replay::All)`.
2. For each `Inserted` / `Replaced` event, calls `decode_message`. If
   the body is not `MessageBody::Reaction`, drops it.
3. Builds a `ReactionEvent` from the decoded message and calls
   `apply_event` on its internal state.
4. If `apply_event` returns `true`, computes the new snapshot for the
   affected `target` and compares its `reactions_signature` against
   `handles.last_target_signatures`. If different, updates the
   stored signature and fires the callback.

Failure modes (decode error, malformed entry) are logged via
`eprintln!` and skipped — same posture as `spawn_tracker` in
`membership.rs`.

## Bridge (`sunset-web-wasm`)

### `Client::new`

After the existing `spawn_tracker` (membership) call, add:

```rust
let reaction_handles = ReactionHandles::default();
sunset_core::reactions::spawn_reaction_tracker(
    store.clone(),
    room.clone(),
    room_fp_hex.clone(),
    reaction_handles.clone(),
);
self.reaction_handles = reaction_handles;
```

### `Client::on_reactions_changed`

Mirrors `Client::on_members_changed` (commit `a0bc05d`):

```rust
pub fn on_reactions_changed(&self, callback: js_sys::Function) {
    let cb = wrap_js_callback(callback);
    *self.reaction_handles.on_reactions_changed.borrow_mut() = Some(cb);
    self.reaction_handles.last_target_signatures.borrow_mut().clear();
    // The tracker's next applied event will see signature ≠ stored
    // and fire the callback with the current state. For an immediate
    // sync of all known targets, iterate `last_target_signatures` and
    // re-fire — left as a small follow-up if it turns out to matter.
}
```

The wrapped JS callback marshals `(target_hash, snapshot)` into:

```ts
type ReactionsChangedPayload = {
    target_hex: string;
    reactions: Map<string, Set<string>>;
    //         emoji        author_pubkey_hex
};
```

### `Client::send_reaction`

```rust
pub fn send_reaction(
    &self,
    target_value_hash_hex: String,
    emoji: String,
    action: String,        // "add" | "remove"
    sent_at_ms: f64,
    nonce_seed: js_sys::Uint8Array,
) -> Result<(), JsValue>
```

Parses `target_value_hash_hex`, parses `action`, calls
`compose_reaction`, and inserts to the local store. The store's
normal sync path fans the entry out; the local `ReactionTracker`
picks it up via the same subscription that handles peer reactions —
no separate path for self.

### Subscription redundancy

The existing message subscription (for `on_message` + auto-receipt)
and the new reaction tracker both subscribe to `<room_fp>/msg/*`,
so each entry decodes twice. Acceptable for v1; the same shape
exists today between `MembershipTracker`'s presence subscription and
any future component that consumes the same prefix. A single
`MessageDispatcher` is a future cleanup if profiles ever flag it.

## Frontend

### Model

`web/src/sunset_web.gleam`:

```gleam
/// Per-target reaction state. Keyed by message id (target value_hash hex).
/// Inner dict: emoji → set of author pubkey hex strings.
/// Whole-snapshot replacement on each `ReactionsChanged` — never
/// partially merge in the FE; the core tracker is the source of truth.
reactions: Dict(String, Dict(String, set.Set(String))),

/// Target id whose picker is currently open, if any.
open_picker_for: option.Option(String),
```

### Msg

```gleam
/// Whole-snapshot delivery from the core ReactionTracker. Empty inner
/// dict means no current reactions for `target`.
ReactionsChanged(target: String, snapshot: Dict(String, set.Set(String)))

/// User clicked a chip (toggle) or picked an emoji from the picker.
SendReaction(target: String, emoji: String, action: ReactionAction)

/// User opened the picker from a message's "+" button.
OpenReactionPicker(target: String)
CloseReactionPicker
```

`ReactionAction` is `Add | Remove`, marshalled as `"add"`/`"remove"`
across the FFI.

### Update branches

```gleam
ReactionsChanged(target, snapshot) ->
  #(Model(..model, reactions: dict.insert(model.reactions, target, snapshot)),
    effect.none())

SendReaction(target, emoji, action) ->
  #(Model(..model, open_picker_for: option.None),
    effect.from(fn(_) {
      sunset.send_reaction(model.client, target, emoji, action_to_string(action))
    }))

OpenReactionPicker(target) ->
  #(Model(..model, open_picker_for: option.Some(target)), effect.none())

CloseReactionPicker ->
  #(Model(..model, open_picker_for: option.None), effect.none())
```

### FFI

`web/src/sunset_web/sunset.gleam` + `sunset.ffi.mjs`:

```gleam
@external(javascript, "./sunset.ffi.mjs", "on_reactions_changed")
pub fn on_reactions_changed(
  client: Client,
  callback: fn(String, Dict(String, set.Set(String))) -> Nil,
) -> Nil

@external(javascript, "./sunset.ffi.mjs", "send_reaction")
pub fn send_reaction(
  client: Client, target_hex: String, emoji: String, action: String,
) -> Nil
```

Wired in the same effect batch as `on_message` / `on_receipt` /
`on_members_changed`.

### UI — chip row

`web/src/sunset_web/views/main_panel.gleam` `message_view` renders a
chip row below the bubble for each `(emoji, authors)` in the
snapshot:

```
┌────────────────────────────┐
│  hey, ready for the call?  │
└────────────────────────────┘
[👍 2] [❤️ 1] [😂 3] [+]
```

- **Chip content**: `emoji × count` (count = `set.size(authors)`).
- **Self-highlight**: filled background if `set.contains(authors,
  self_pubkey_hex)`, outlined otherwise.
- **Tap behavior**: dispatches `SendReaction(target, emoji, action)`,
  where `action = if set.contains(authors, self) then Remove else Add`.
- **Empty state**: no chips. The "+" button appears on hover (desktop)
  / long-press (mobile) to keep idle bubbles visually quiet.

### UI — picker

`OpenReactionPicker(target)` opens a popover (desktop) or bottom sheet
(mobile, reusing the `feae973` pattern):

```
╭──────────────────────────────╮
│  👍  ❤️  😂  🎉  😮  😢      │  ← quick row (rendered by us)
│  ─────────────────────────   │
│  [emoji-picker web component]│  ← lazy-loaded
╰──────────────────────────────╯
```

- **Quick row**: fixed `[👍 ❤️ 😂 🎉 😮 😢]`. Tap →
  `SendReaction(target, emoji, Add)`.
- **Full picker**: `emoji-picker-element` web component, dynamic-
  imported on first picker open. In Lustre it slots in as
  `element.element("emoji-picker", [...], [])` with an `emoji-click`
  event listener that dispatches `SendReaction(target, payload.unicode, Add)`.
- Closing: click outside, press Escape, or pick an emoji — all
  dispatch `CloseReactionPicker` (the `SendReaction` update branch
  also clears `open_picker_for`).

### Dependency

Adds one runtime dep: `emoji-picker-element` (~30 KB minified, MIT,
no transitive runtime deps). Per the hermeticity rule
(`CLAUDE.md`), `flake.nix` updates so `nix build` for `web/`
resolves the package without any implicit `npm install` step.

## Edge cases

- **Reaction to a not-yet-seen message.** Tracker keys state by
  `target` regardless of whether the target message is locally known.
  The FE's `reactions` dict picks it up; whenever the target message
  later renders, its chip row reads from the dict. No buffering
  needed.
- **Multi-device race on the same identity.** Phone Adds 👍, laptop
  (which hasn't seen the Add yet) Adds 👍 too. LWW per `(author,
  target, emoji)` collapses them to a single Add. Opposite case
  (laptop Adds, phone Removes) resolves to whichever has the later
  `sent_at_ms` (tiebreak `value_hash`).
- **Clock skew.** LWW is by `sent_at_ms`. Same exposure as text
  messages today; not a new problem.
- **Removal-without-prior-Add.** Tracker accepts it; LWW winner
  determines final state. Standalone Remove yields an empty snapshot
  for that emoji.
- **Spam.** No rate-limit in v1; same posture as text messages.
- **Oversized emoji.** Compose rejects with `EmojiTooLong { len }`;
  decode rejects defensively. Picker only emits valid emoji, so users
  never hit the cap.

## Loop avoidance

Trivial: reactions are user-driven only. The bridge never
auto-emits a reaction in response to any incoming entry. Replay of
historical entries simply re-applies them through the LWW fold,
converging to the same state.

## Testing

### Core (`sunset-core`)

- Unit: `compose_reaction` round-trips through `decode_message`;
  body equals `Reaction { for_value_hash, emoji, action }`.
- Unit: `compose_reaction` rejects 65-byte emoji with
  `EmojiTooLong { len: 65 }`.
- Unit: `decode_message` rejects a manually-crafted oversized emoji
  entry.
- Unit: a reaction signed by Alice cannot be decoded as one by Bob
  (signature gate).
- Unit (pure): `apply_event` LWW — out-of-order events for the same
  `(author, target, emoji)` always converge to the highest
  `(sent_at_ms, value_hash)`. Add/Remove/Add at descending
  timestamps yields the latest action.
- Unit (pure): `derive_snapshot` returns the expected `(emoji →
  set of authors)` map; `Remove` winners omit the author.
- Unit (pure): `reactions_signature` ignores irrelevant ordering;
  only changes when the snapshot semantically changes.
- Integration: `spawn_reaction_tracker` against an in-memory store —
  write three `Reaction` entries, observe one debounced
  `on_reactions_changed` per logical state change.
- Wire-format pin: hex test vectors for `Reaction::Add` and
  `Reaction::Remove`.

### Bridge (`sunset-web-wasm`)

- Integration: two-engine test where Alice reacts 👍, Bob's
  `Client::on_reactions_changed` fires with `{👍: {alice_pubkey}}`
  for the message id; Alice removes, callback fires again with `{}`.
- Integration: `Client::send_reaction("…", "🎉", "add", …)` produces
  an entry that decodes back to `MessageBody::Reaction { Add, "🎉",
  … }` and is observable through `on_reactions_changed`.

### Frontend (Playwright)

- Two browser tabs in the same room. Alice taps 👍 on Bob's message
  → chip appears in both tabs with count 1, Alice's tab shows it
  filled, Bob's tab shows it outlined.
- Alice taps her own 👍 chip → chip disappears in both tabs.
- Alice opens picker, picks 🦊 from the full picker → chip appears
  with 🦊 × 1.
- Self-reaction: Alice reacts to her own message; chip renders,
  count is 1, filled.
- Mobile viewport: picker opens as a bottom sheet, not a popover.

## Open questions

None at design time. Implementation may surface decisions around
exact chip styling (filled vs. ring, padding, gap), the picker's
open/close transition, and whether the quick-row's six emoji
become user-configurable later. UI polish to resolve in code.
