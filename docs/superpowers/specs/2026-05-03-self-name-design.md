# Self-name (user-chosen display name) — design

Date: 2026-05-03
Status: design — implementation pending plan

## Summary

Let the user pick a display name shown to peers in place of `short_pubkey`. The name is an application-level field carried inside each peer's existing presence heartbeats, so it rides the existing replication path with no new namespace, filter, or sync wiring. Receivers attach the name to the corresponding `Member` in the membership tracker; the web client renders names by pubkey lookup at view time, so a rename is reflected on every existing surface — member rail, "you" row, peer-status popover, receipts list, and the author column on every already-rendered message — within one heartbeat round-trip.

## Scope

In scope:

- A name field in the existing settings popover (left rail, "you" row).
- Local persistence in `localStorage["sunset/self-name"]`.
- Wire format: extend the presence-heartbeat `ContentBlock.data` from empty to a postcard-encoded `PresenceBody { name: Option<String> }`.
- Membership tracker reads `name` and exposes it on `Member` (Rust) / `MemberJs` (wasm).
- `domain.Message` switches `author: String` → `author_pubkey: BitArray`. View renders the author via a render-time `pubkey → name` dict lookup, falling back to `short_pubkey(pk)`.
- Same render-time lookup powers receipts and any other site that names a peer.

Out of scope (deferred):

- Per-room name overrides. Identity is global; one name covers every room. Per-room can be added later without breaking the wire (presence body is the natural place for a future `room_overrides` field).
- Avatars, status text, bios, "is typing." `PresenceBody` is sized for the current need; we may extend it later but do not pre-design fields.
- Collision handling beyond "show whatever the peer claims." The peer-status popover already exposes the pubkey for verification; impersonation is a social/UX problem disambiguation does not solve.
- TUI / mod / native clients. The wire format is portable, but only the web client surfaces the field in v1.

## Backwards compatibility

None. Sunset is pre-release; we change the presence body format unconditionally and let any older peers' empty bodies fail to decode. The decode path logs at warn and treats the peer as nameless rather than dropping them, but no special-case keeps the legacy empty body path alive.

## Architecture

```
[settings popover name input]
    ↓ keystroke (debounced ~300ms)
[Gleam Model.self_name + localStorage("sunset/self-name")]
    ↓
[wasm Client.set_self_name(name)]
    ↓
[membership::publisher: swap current_name handle, fire Notify]
    ↓
[publish_once: encode PresenceBody → ContentBlock.data → sign → store.insert]
    ↓ replicates over sunset-sync
[peer's local store: Inserted/Replaced event]
    ↓
[membership::tracker reads block, decodes PresenceBody, updates names map]
    ↓
[derive_members fills Member.name; signature includes name; callback fires]
    ↓
[wasm MemberJs.name() returns the name]
    ↓
[Gleam MembersUpdated → Model.name_map: Dict(hex_pk, name)]
    ↓                                                       ↓
[member rail / you row / popovers / receipts]   [message render: display_name(name_map, author_pk)]
```

The publisher gains a small piece of state (`current_name: Rc<RefCell<Option<String>>>`) and a `Notify` so that `update_name` triggers an immediate publish instead of waiting for the next interval. Idempotent — a no-op rename does not re-publish.

`Identity` stays pure crypto. Display name lives at the membership layer, not the identity layer — it is data the user publishes, not data the keypair carries.

## Wire format

In `crates/sunset-core/src/membership` (new module `body.rs` or co-located with `publisher.rs`):

```rust
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct PresenceBody {
    /// User-chosen display name. None ⇒ no name set; receivers fall back
    /// to short_pubkey rendering. Trimmed and length-limited (64 chars,
    /// counted by `chars()`) by the publisher; receivers do not re-validate.
    pub name: Option<String>,
}
```

Carried as the `ContentBlock { data: postcard::to_stdvec(&body).unwrap(), references: vec![] }` of each `<room_fp>/presence/<my_pk>` entry. The signed-entry `value_hash` already covers the block, so integrity end-to-end is unchanged.

Length limit: publisher trims with `String::trim` and truncates with `chars().take(64).collect::<String>()`. Empty after trim ⇒ `name: None`. The settings input enforces 64 first via HTML `maxlength`; publisher truncation is defense in depth.

Decode policy on the receive side: on any postcard decode error, `tracing::warn!(error = %e, peer = %hex_pk, "presence body decode failed")` and treat the peer as `name: None`. No retry, no special-casing of empty input.

Forward extensibility: postcard does not auto-tolerate added fields. Any future addition must be `Option<T>` with `#[serde(default)]` and the postcard-encoding test vector for the v1 shape (`PresenceBody { name: Some("alice") }`) is pinned in the unit tests so accidental wire drift fails CI.

The frozen `ContentBlock::hash()` test vector in `sunset-store::types` is unaffected — that pins the envelope format, not the application-level body bytes inside `data`.

## Components

### `crates/sunset-core/src/membership/mod.rs`

- `Member` gains `pub name: Option<String>`.
- `MemberSig` widens to `Vec<(Vec<u8>, Presence, ConnectionMode, Option<String>)>` so name changes cross the debounce.
- `derive_members()` takes a new `&HashMap<PeerId, Option<String>>` (the names map) alongside the existing presence and kinds maps, and copies `name` onto each `Member`. Self's name comes from the same map (publisher inserts its own entry; tracker reads it back like any other peer's, keeping a single code path; no special "self" branch).
- Tracker task: on each presence Inserted/Replaced event, attempt to fetch the entry's block via `store.get_content(&value_hash)` and decode `PresenceBody` into an `Rc<RefCell<HashMap<PeerId, Option<String>>>>` names map alongside `presence_map`.
  - Block-fetch contract: sync may deliver the entry before its block (the store contract explicitly allows lazy dangling refs — see `CLAUDE.md` "Lazy dangling refs allowed"). On `Ok(None)` (block missing), the peer is recorded with `name: None` and added to a `pending_block_fetches: HashSet<PeerId>` keyed on the entry's `value_hash`.
  - The existing presence subscription already receives `Event::BlobAdded { hash }` (BlobAdded is broadcast to every subscriber regardless of filter — see `crates/sunset-store-memory/src/subscription.rs`). The tracker's `presence_sub` select arm gains another branch matching `BlobAdded`: if `hash` matches any value in `pending_block_fetches`, retry the fetch + decode and update the names map. No new subscription needed.
  - Decode failure (block present but bytes don't decode) ⇒ `tracing::warn!` + `name: None`. Not retried.
- Existing tests in this file are extended to cover the names map; new tests cover decode failure + `MemberSig` change-on-rename + late-arriving block (insert entry first, observe `name: None`; insert block, observe rename).
- Existing tests in this file are extended to cover the names map; new tests cover decode failure + `MemberSig` change-on-rename.

### `crates/sunset-core/src/membership/publisher.rs`

- New shared handle `current_name: Rc<RefCell<Option<String>>>` returned from `spawn_publisher`.
- New shared `Notify` (or equivalent — `tokio::sync::Notify` is available cross-target via the existing dep tree, otherwise an `mpsc::UnboundedSender<()>`) so `update_name` can trigger an immediate publish.
- Publisher loop selects on `sleep(interval_ms)` *or* `notify.notified()`, then calls `publish_once`.
- `publish_once`:
  - Reads `current_name.borrow().clone()` into a `PresenceBody`.
  - Encodes with `postcard::to_stdvec`.
  - Builds `ContentBlock { data, references: vec![] }`.
  - Recomputes `value_hash` (block.hash()).
  - Signs over the canonical entry payload.
  - Inserts (`store.insert(entry, Some(block))`).
  - Hash changes when name changes, satisfying the store's `entry.value_hash == blob.hash()` invariant automatically.
- `update_name(handle, new_name)`:
  - Trims and truncates `new_name` (`chars().take(64)`); empty after trim ⇒ `None`.
  - If equal to current value: return without notifying.
  - Otherwise: replace the `RefCell` value, `notify.notify_one()`.

### `crates/sunset-web-wasm/src/members.rs`

- `MemberJs` gains a wasm-bindgen getter `pub fn name(&self) -> Option<String>`. Wasm-bindgen exposes `Option<String>` as `string | undefined` on the JS side.

### `crates/sunset-web-wasm/src/client.rs`

- `Client` keeps a `Vec<PublisherHandle>` (one per room) — the publishers it spawns on `open_room` already live here; we just expose them via `update_name`.
- New constructor parameter: `initial_name: Option<String>`, threaded into the first `spawn_publisher` call so the first heartbeat carries the name (avoids a "nameless first heartbeat → named second heartbeat" flicker).
- New API: `pub async fn set_self_name(&self, name: Option<String>)` — calls `publisher::update_name` for every publisher. (One per joined room. Publishers per room is a sunset-core invariant; we do not collapse it here.)

### `crates/sunset-core/src/identity.rs`

Untouched.

### `web/src/sunset_web/storage.gleam` + `storage.ffi.mjs`

- `read_self_name() -> String` — reads `localStorage["sunset/self-name"]`, returns `""` when unset.
- `write_self_name(name: String) -> Nil` — writes; empty string clears.

### `web/src/sunset_web/sunset.ffi.mjs` + `sunset.gleam`

- `set_self_name(client, name: String, callback)` — JS shim translates `""` → `undefined` and calls `Client.set_self_name`. Callback fires when the underlying promise resolves.
- `mem_name(member) -> Option(String)` — wraps the new wasm getter (`undefined → None`, string → `Some`).
- `create_client` constructor accepts the initial name (the bootstrap reads from localStorage and passes it through).

### `web/src/sunset_web/domain.gleam`

- `Message.author: String` → `Message.author_pubkey: BitArray`. The cached `initials: String` stays — initials are derived from the pubkey and are stable across renames.
- `Member.name` keeps its `String` shape — `map_members` uses `mem_name(m)` if `Some`, else `short_pubkey(pk)`. Single source of truth used by every name-rendering site.

### `web/src/sunset_web.gleam`

- `Model` gains:
  - `self_name: String` — display value of the input. `""` means unset.
  - `self_name_token: Int` — debounce token (incremented on every keystroke).
  - `name_map: Dict(String /* hex pk */, String /* current display name */)`.
- New `Msg`s:
  - `UpdateSelfName(String)` — input handler. Increments token, schedules a `SelfNameCommit(value, token)` via `effect` after 300ms.
  - `SelfNameCommit(String, Int)` — fires after 300ms idle. If `token != model.self_name_token`, drop. Otherwise: `storage.write_self_name(value)` + `sunset.set_self_name(client, value, ...)`.
- `MembersUpdated(room, members)` handler builds `name_map` from `members`. Across rooms: last-write-wins is fine because identity is global → all rooms agree on a peer's name once they receive the latest heartbeat. Implementation: fold over every room's `members` list and overlay into a single dict.
- `IncomingMsg` handler stops snapping `author = short_pubkey(pk)`; just stores `author_pubkey`.
- New view helper `display_name(name_map: Dict(String, String), pk: BitArray) -> String`: hex-encode `pk`, dict lookup, fall back to `short_pubkey(pk)`.

### `web/src/sunset_web/views/settings_popover.gleam`

- New `name_section` rendered above `theme_section`:
  - Single `<input type="text">` bound to `self_name`.
  - `placeholder="Set a name (optional)"`.
  - `maxlength="64"`.
  - `data-testid="settings-name-input"`.
  - `event.on_input(UpdateSelfName)`.
  - Below the input: a faint helper line `"Visible to peers in your rooms."`.
- No save button; debounce handles commit.
- Existing theme + reset sections unchanged.

### `web/src/sunset_web/views/rooms.gleam`

`you_row` already pulls `your_name` from the self-Member. No change needed — it tracks live edits automatically once the round-trip completes (input → publish → tracker → MembersUpdated → name_map → next render → Member.name → you_row).

### Other view files

`main_panel`, `details_panel`, `peer_status_popover`, anywhere that currently reads `m.author`: update to call `display_name(model.name_map, m.author_pubkey)`. Implementation choice: pre-resolve once per render into a derived `messages_for_view: List(MessageView)` rather than threading `name_map` through every view function. Single helper, used everywhere consistently.

## Edge cases

- **Same person in multiple rooms with different names** — cannot happen by construction. `set_self_name` updates every room's publisher; identity is global; all rooms converge.
- **Two peers pick the same name** — render the name. Pubkey verification is one click away in the peer-status popover. (Q2/A.)
- **Name change races a message** — both signed by the same key; whichever arrives first, the next `MembersUpdated` rebuilds `name_map` and everything re-renders. No ordering invariant to defend.
- **Peer renames, goes offline, comes back later** — tracker drops them when their presence expires, drops their `name_map` entry on the next derivation. On return, the new heartbeat re-seeds. Past messages from them render as `short_pubkey` during the gap; this is the design.
- **Empty input** — publishes `name: None`; peers see `short_pubkey` again.
- **Whitespace-only name** — trimmed in publisher; treated as `None`.
- **Surrogate-pair / emoji names** — `chars().take(64)` is grapheme-cluster-naive; `"👨‍👩‍👧‍👦"` counts as 7 chars. Tradeoff: we do not pull in `unicode-segmentation` for one field. UTF-8 byte boundaries are never split.
- **Decode failure** — peer treated as `name: None`, warn log. Does not drop the peer from the member list.
- **Entry arrives before block** — peer initially renders as `name: None`; the tracker's BlobAdded subscription retries the fetch when the block lands and the name appears. Observable as a brief flicker on first appearance of a remote peer; same shape as any other lazy-blob-fetch flow in the system.

## Testing

### Unit (Rust, `sunset-core::membership`)

- `derive_members` fills `Member.name` from the names map; absence ⇒ `None`.
- `members_signature` differs when only `name` differs (so callback fires on rename).
- `PresenceBody` postcard round-trip: `None`, ASCII name, multi-byte UTF-8 name, exactly-64-char name.
- Pinned hex test vector for `postcard::to_stdvec(&PresenceBody { name: Some("alice") })` so accidental wire drift fails CI.
- Tracker test: insert presence entry with `name: Some("alice")`, observe Member with that name; insert a second with `name: Some("alice2")`, observe replacement.
- Tracker test: insert presence entry whose body is garbage bytes; tracker logs and continues, peer renders as `name: None`.
- Tracker test: insert presence entry without its block (simulating sync entry-before-block ordering); peer renders as `name: None`. Insert the matching block; tracker re-derives via BlobAdded and the peer's name appears.

### Unit (Rust, `publisher`)

- `publish_once` writes a body whose decoded `PresenceBody.name` matches the current handle value.
- `update_name` triggers immediate publish (timer-independent test using the `Notify` directly).
- `update_name` with the same value is idempotent (no extra `store.insert`).
- `update_name("  ")` (whitespace only) ⇒ `None`.
- `update_name("a".repeat(100))` ⇒ truncated to 64 chars.

### Unit (Gleam)

- `display_name` returns dict value when present, `short_pubkey(pk)` fallback when absent.
- Debounce token logic: stale `SelfNameCommit` (token mismatch) is dropped; latest-token commit is honored.

### wasm-bindgen integration test (`sunset-web-wasm/tests/`)

- Two clients A, B in the same room. A calls `set_self_name("Alice")`. Within one heartbeat tick, `B.client.members()` includes A with `name() == Some("Alice")`.
- A calls `set_self_name(None)`. Within one heartbeat tick, `B.client.members()` includes A with `name() == None`.

### Playwright e2e (`web/e2e/`)

- Open settings → type "Alice" → blur → reload page → name persists in input + visible on you-row.
- Two peer windows: peer 1 sends a message ("hi"), peer 2 sees it authored as `short_pubkey`. Peer 1 sets name to "Alice"; peer 2's already-rendered message re-authors to "Alice" within the heartbeat round-trip.
- Peer 1 clears the name; peer 2 reverts to `short_pubkey` rendering.

### Conformance suite

`sunset-store::test_helpers` is unaffected — the store contract does not change.

## Non-goals / explicit deferrals

- No name uniqueness checks, server-side or otherwise. There is no server.
- No name-change rate limiting beyond the natural debounce. `update_name` is cheap (one store insert) and a malicious user can already publish heartbeats at the publisher's interval; the threat is bounded by the existing rate.
- No "verified name" badge for known-good identities. Out of v1.
- No per-room overrides — see Scope.
- No migration from the existing empty body — see Backwards compatibility.

## Open questions

None.

## References

- Architecture: `docs/superpowers/specs/2026-04-25-sunset-chat-architecture-design.md`
- Membership tracker: `crates/sunset-core/src/membership/mod.rs`
- Presence publisher: `crates/sunset-core/src/membership/publisher.rs`
- Settings popover: `web/src/sunset_web/views/settings_popover.gleam`
- Web Model: `web/src/sunset_web.gleam`
