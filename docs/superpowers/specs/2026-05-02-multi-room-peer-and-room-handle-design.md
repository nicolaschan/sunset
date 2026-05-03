# Multi-Room Peer and RoomHandle Design

**Date:** 2026-05-02
**Scope:** Decouple "the running peer" from "a single room" so the web client (and every future host — TUI, Minecraft mod, native relay) can have multiple rooms open concurrently against one shared engine, store, and transport stack. The web client's room rail starts actually routing messages to the room the user has selected. Multi-room logic lands in `sunset-core`; the wasm-bridge crate becomes a thin `wasm-bindgen` veneer over it.
**Out of scope (explicit):** channels (no `MessageBody` wire-format change), persistent cache for `K_room` derivations, per-room read-receipt cursors / unread counts, voice scoping to channels.

## Goal

Today the web client opens a single hardcoded room (`"sunset-demo"`, `sunset_web.gleam:574`) at page load. The room rail in the UI swaps URL fragments and pretty names, but the underlying `sunset_core::Room` — and therefore the room fingerprint, encryption key, replication subscription, and presence/message streams — never changes. Picking `#dusk-collective` vs `#design-crit` shows different placeholder text and nothing else; both rooms read and write the same crypto room.

The user-visible goal: clicking a room in the rail makes the engine actually use that room. Joined rooms keep ticking in the background so unread counts can grow and switching back to an open room is instant.

The architectural goal: `sunset-core::Peer` becomes the host-agnostic "running sunset peer" entity, parameterized over a transport stack and a `Spawner` for per-room background tasks. `sunset-core::OpenRoom` is the per-room handle, returned by `Peer::open_room`. The wasm crate's `Client` shrinks to identity-from-localStorage + voice + a `WasmSpawner`; everything else delegates to core. When the TUI plan starts, it gets multi-room "for free."

## Non-goals (and why)

- **Channels.** The user's prompt mentioned both, but channels need a wire-format change to `MessageBody::Text(String)` (a frozen v1 vector lives in `crates/sunset-core/src/crypto/envelope.rs:213`). Worth a separate brainstorm. UI keeps `current_channel` per-room as a remembered selection; sending and receiving ignore it.
- **Persistent K_room cache.** Argon2id with production params is the slow piece of `Room::open`. We mitigate the multi-room page-load cost via stagger only; persistent caching (localStorage / IndexedDB keyed by room name) is a sensible follow-up.
- **Per-room unread counts.** Useful, but derives from a "last seen at sequence" cursor that the Gleam UI doesn't have today. Not in scope.
- **Voice channels per room.** Voice is fixture-only and orthogonal.

## Architecture

```
sunset-core::Peer<T: Transport, St: Store>           [NEW — moves Client logic out of wasm crate]
   identity, store, engine, supervisor, transport stack, relay_status (global)
   open_rooms: HashMap<RoomFingerprint, Weak<RoomState>>
   spawner: Spawner                                  [NEW trait; Wasm + Tokio impls]
   ↓ open_room(name) -> OpenRoom
sunset-core::OpenRoom                                 [NEW]
   room: Arc<Room>
   subscription_id, renewal task
   message decode task (room_messages_filter)
   presence_publisher + membership tracker
   per-room RelaySignaler (registered with shared transport's MultiRoomSignaler)
   on_message / on_receipt / on_members / on_relay_status callbacks
   ↓ wasm-bindgen veneer
sunset-web-wasm::Client (thin)                       [shrinks: ~Spawner + identity + voice]
sunset-web-wasm::RoomHandle                          [NEW — wraps OpenRoom]
   ↓ FFI (sunset.gleam, sunset.ffi.mjs)
Gleam Lustre app
   Model.rooms: Dict(String, RoomState)              [per-room state replaces flat fields]
   Model.client: Option(ClientHandle)                [identity + relay only]
```

The `Spawner` abstraction is the cross-host glue. Per-room background tasks (subscription renewal, message decode loop, presence publisher) need a runtime to live in. Today these all use `wasm_bindgen_futures::spawn_local` directly. After this change:

```rust
pub trait Spawner: 'static {
    fn spawn_local(&self, fut: Pin<Box<dyn Future<Output = ()> + 'static>>);
}
```

`sunset-web-wasm` provides `WasmSpawner`. The future TUI provides one wrapping `tokio::task::spawn_local`. The trait sits in `sunset-core` next to `Peer`. We introduce it now even though no other host exists yet — defining it later means rewriting every call site, and the trait is small.

## Why move all of this to core

Three of the four upcoming hosts (TUI, Minecraft mod, native relay) will all want a multi-room peer. The Discord/Slack-style "rooms keep updating" UX is not a web concern — it's a property of the underlying peer. If `Client` stays in `sunset-web-wasm`, every other host re-implements the same wiring and the same multi-room logic. The wasm crate is meant to be a thin browser bridge per the architecture spec; the multi-room shape belongs alongside `Room` and `membership` in core.

Two existing wasm-only modules move to core as part of this:

- `presence_publisher::spawn_publisher` → `sunset_core::membership::spawn_publisher` (sits next to the existing `spawn_tracker`). Today the only host-specific piece is the `wasm_bindgen_futures::spawn_local` call; after the `Spawner` trait, the function is fully host-agnostic.
- `RelaySignaler` → `sunset_core::signaling::RelaySignaler`. The `web_sys::console` calls swap for `tracing` (already a workspace dep), and the spawn calls go through the `Spawner`.

## Components

### `sunset_core::Peer<T, St>` (new — `crates/sunset-core/src/peer/mod.rs`)

```rust
pub struct Peer<T: Transport, St: Store> {
    identity: Identity,
    store: Arc<St>,
    engine: Arc<SyncEngine<St, T>>,
    supervisor: Arc<PeerSupervisor<St, T>>,
    relay_status: Arc<RelayStatusCell>,
    spawner: Box<dyn Spawner>,
    open_rooms: Mutex<HashMap<RoomFingerprint, Weak<RoomState>>>,
    rtc_signaler_dispatcher: Arc<MultiRoomSignaler>, // if rtc transport in use
}

impl<T, St> Peer<T, St> {
    pub fn new(
        identity: Identity,
        store: Arc<St>,
        transport: T,
        rtc_signaler_dispatcher: Arc<MultiRoomSignaler>,
        spawner: Box<dyn Spawner>,
    ) -> Self;

    pub async fn add_relay(&self, addr: PeerAddr) -> Result<()>;
    pub async fn open_room(self: &Arc<Self>, room_name: &str) -> Result<OpenRoom>;
    pub fn close_room(&self, fp: RoomFingerprint);   // removes weak; OpenRoom drop does the actual cleanup
    pub fn relay_status(&self) -> String;
    pub fn public_key(&self) -> [u8; 32];
}
```

`open_room` is idempotent on room fingerprint: if a strong `OpenRoom` already exists in the registry, return another reference to the same `RoomState` rather than constructing a duplicate. This is important because `JoinRoom(name)` for an already-joined room must not stand up a second engine task.

`close_room` removes the `Weak` from the registry, but the actual cleanup happens when the last `Arc<RoomState>` is dropped — so JS-side keeping a `RoomHandle` alive will keep the room running even if the Rust side "closes" it. In practice, callers drop the handle to close.

### `sunset_core::OpenRoom` (new — same module)

```rust
pub struct OpenRoom {
    inner: Arc<RoomState>,
}

struct RoomState {
    room: Arc<Room>,
    peer_weak: Weak<Peer<T, St>>,         // for store/engine/identity access
    subscription_id: SubscriptionId,
    presence_started: AtomicBool,
    tracker_handles: Arc<TrackerHandles>,
    on_message: Mutex<Option<MessageCallback>>,
    on_receipt: Mutex<Option<ReceiptCallback>>,
    cancel: CancellationToken,             // fires on Drop; ends background tasks
}

impl OpenRoom {
    pub async fn send_text(&self, body: String, sent_at_ms: u64) -> Result<Hash>;
    pub fn start_presence(&self, interval_ms: u64, ttl_ms: u64, refresh_ms: u64);
    pub async fn connect_direct(&self, peer_pubkey: [u8; 32]) -> Result<()>;
    pub fn on_message<F: Fn(&DecodedMessage) + 'static>(&self, cb: F);
    pub fn on_receipt<F: Fn(Hash, &VerifyingKey) + 'static>(&self, cb: F);
    pub fn on_members_changed<F: Fn(&[Member]) + 'static>(&self, cb: F);
    pub fn on_relay_status_changed<F: Fn(&str) + 'static>(&self, cb: F); // mirrors Peer's global
    pub fn fingerprint(&self) -> RoomFingerprint;
}

impl Drop for RoomState {
    fn drop(&mut self) {
        self.cancel.cancel();
        // unregister signaler from MultiRoomSignaler (best-effort via peer_weak)
    }
}
```

`subscribe_messages` / `subscribe_members` could alternatively be `Stream`-returning — for the wasm bridge, callback-style is more convenient (matches `js_sys::Function`). For native hosts that prefer `Stream`, we can add stream-returning variants later without breaking the callback API.

### `sunset_core::signaling::RelaySignaler` (moved — `crates/sunset-core/src/signaling/relay_signaler.rs`)

Moves verbatim from `crates/sunset-web-wasm/src/relay_signaler.rs` with two changes:
1. `web_sys::console::error_1(...)` → `tracing::error!(...)`. Workspace already depends on `tracing`.
2. `wasm_bindgen_futures::spawn_local(...)` → `spawner.spawn_local(...)`. Constructor gains a `Box<dyn Spawner>` parameter.

The Noise_KK signaling logic, the `Signaler` trait impl, the `signaling_filter` helper — all unchanged.

### `sunset_core::signaling::MultiRoomSignaler` (new — same module)

The `Signaler` trait (`crates/sunset-sync/src/signaler.rs`) is room-agnostic — `send(SignalMessage)` carries `(from, to, seq, payload)` with no room context, and `recv()` just awaits the next inbound. The transport that uses it (`WebRtcRawTransport`) likewise doesn't know about rooms. So `MultiRoomSignaler` doesn't dispatch by room id from the message — it fans `recv` across all registered per-room signalers and picks one of them for each `send`.

```rust
pub struct MultiRoomSignaler {
    by_room: RwLock<HashMap<RoomFingerprint, Arc<RelaySignaler>>>,
}

impl MultiRoomSignaler {
    pub fn new() -> Arc<Self>;
    pub fn register(&self, room_fp: RoomFingerprint, signaler: Arc<RelaySignaler>);
    pub fn unregister(&self, room_fp: RoomFingerprint);
}

impl Signaler for MultiRoomSignaler {
    async fn send(&self, msg: SignalMessage) -> Result<()> {
        // Pick any one registered per-room signaler (first in iteration
        // order). The receiver subscribes to all its rooms via its own
        // MultiRoomSignaler, so any single room works as the carrier as
        // long as we both have it open. If no rooms are open, send fails
        // — matches "you can't WebRTC-direct to a peer with no shared
        // open room," which is the correct semantics.
    }
    async fn recv(&self) -> Result<SignalMessage> {
        // select! across every registered per-room signaler's recv.
        // First to fire wins. New registrations during a long-running
        // recv require an internal wakeup channel — see implementation
        // plan for the precise mechanism.
    }
}
```

The shared `WebRtcRawTransport` is constructed with one `MultiRoomSignaler` (instead of a single `RelaySignaler`). Each `OpenRoom` creates its per-room `RelaySignaler` (which keeps its own per-room subscription on `<fp>/webrtc/`) and calls `dispatcher.register(fp, signaler)`. Drop calls `unregister`.

Caveat — if peers A and B both have rooms X and Y open, A's `MultiRoomSignaler` may pick room X to send through while B's picks room Y; both messages still reach the other side because each side's dispatcher is `recv`-ing from both rooms. The pair will end up with one direct WebRTC connection (not two) because `WebRtcRawTransport` deduplicates by `(from, to)`. The "two connections per peer pair across rooms" wart noted earlier was therefore overstated for *this* design — it would only show up if we kept N independent `WebRtcRawTransport` instances. With one shared transport plus a `MultiRoomSignaler`, we get one connection per peer pair for free.

### `sunset_core::membership::spawn_publisher` (moved)

Moved from `crates/sunset-web-wasm/src/presence_publisher.rs`. Same change set as `RelaySignaler`: `web_sys` → `tracing`, `spawn_local` → `Spawner`.

### `sunset_core::Spawner` (new trait — `crates/sunset-core/src/spawner.rs`)

```rust
use std::future::Future;
use std::pin::Pin;

pub trait Spawner: Send + Sync + 'static {
    fn spawn_local(&self, fut: Pin<Box<dyn Future<Output = ()> + 'static>>);
}
```

For the wasm crate's `Send + Sync` constraint: the trait object is `Send + Sync` so a single spawner can be shared, but the futures it spawns are `?Send` (matching the existing `#[async_trait(?Send)]` posture of the data plane). This mirrors how `SignatureVerifier` is the one `Send + Sync` exception in `sunset-store`.

`sunset-web-wasm::WasmSpawner`:

```rust
pub struct WasmSpawner;

impl Spawner for WasmSpawner {
    fn spawn_local(&self, fut: Pin<Box<dyn Future<Output = ()> + 'static>>) {
        wasm_bindgen_futures::spawn_local(fut);
    }
}
```

### `sunset-web-wasm::Client` (slimmed)

```rust
#[wasm_bindgen]
pub struct Client {
    inner: Arc<Peer<MultiTransport<WsT, RtcT>, MemoryStore>>,
    identity: Identity,                  // exposed for public_key getter
    voice: VoiceCell,                    // unchanged
}

#[wasm_bindgen]
impl Client {
    #[wasm_bindgen(constructor)]
    pub fn new(seed: &[u8]) -> Result<Client, JsError>; // no room_name parameter

    pub async fn add_relay(&self, url: String) -> Result<(), JsError>;
    pub fn relay_status(&self) -> String;
    #[wasm_bindgen(getter)]
    pub fn public_key(&self) -> Vec<u8>;

    pub async fn open_room(&self, name: &str) -> Result<RoomHandle, JsError>;

    // voice methods — unchanged
}
```

### `sunset-web-wasm::RoomHandle` (new)

```rust
#[wasm_bindgen]
pub struct RoomHandle {
    inner: OpenRoom,
}

#[wasm_bindgen]
impl RoomHandle {
    pub async fn send_message(
        &self, body: String, sent_at_ms: f64, nonce_seed: Vec<u8>,
    ) -> Result<String, JsError>;

    pub fn on_message(&self, cb: js_sys::Function);
    pub fn on_receipt(&self, cb: js_sys::Function);
    pub fn on_members_changed(&self, cb: js_sys::Function);
    pub fn on_relay_status_changed(&self, cb: js_sys::Function);

    pub async fn start_presence(&self, interval_ms: u32, ttl_ms: u32, refresh_ms: u32);
    pub async fn connect_direct(&self, peer_pubkey: &[u8]) -> Result<(), JsError>;
    pub fn peer_connection_mode(&self, peer_pubkey: &[u8]) -> String;
    pub fn close(self);                  // explicit; alternatively rely on JS GC
}
```

`relay_status` is mirrored on the handle (sourced from the shared `Peer`) so the JS side has one consistent place to subscribe per room. Mild redundancy; uniformity is worth it.

### Per-room subscriptions, decode tasks, presence — on a shared engine

One `MemoryStore` holds entries for every open room (they self-namespace via `name = <room_fp>/...`; GC reachability still works across the whole store because `ContentBlock.references` is content-addressed, not room-namespaced). One `SyncEngine` runs against that store.

**Subscription** — `OpenRoom` construction calls `engine.publish_subscription(room_filter(&room), Duration::from_secs(3600))` and stores the `SubscriptionId`. A renewal task spawned via the `Spawner` republishes at `ttl/2`. On drop, the cancellation token fires and the renewal task ends; the subscription expires naturally at the relay. We do not need an explicit revoke.

**Message decode task** — one spawned future per open room, using `store.subscribe(room_messages_filter(&room), Replay::All)`. Logic identical to today's `spawn_message_subscription` (`crates/sunset-web-wasm/src/client.rs:369`), lifted into core. The `acked: HashSet<Hash>` for receipt dedup stays per-room.

**Presence publisher + tracker** — both already take `room_fp_hex`; both move into core. `OpenRoom::start_presence` constructs them.

**Auto-ack receipts** — folds into the per-room decode task as today; uses `compose_receipt(identity, &room, ...)`.

### `sunset_web` Lustre model changes

```gleam
pub type RoomState {
  RoomState(
    handle: ClientRoomHandle,
    messages: List(domain.Message),
    members: List(domain.Member),
    receipts: Dict(String, Set(String)),
    reactions: Dict(String, List(Reaction)),
    current_channel: ChannelId,        // remembered per-room; ignored on the wire
    draft: String,
    selected_msg_id: Option(String),
    reacting_to: Option(String),
    sheet: Option(domain.Sheet),
    peer_status_popover: Option(domain.MemberId),
  )
}

pub type Model {
  Model(
    // global
    mode: Mode,
    view: View,
    viewport: domain.Viewport,
    joined_rooms: List(String),
    rooms_collapsed: Bool,
    landing_input: String,
    sidebar_search: String,
    dragging_room: Option(String),
    drag_over_room: Option(String),
    voice_settings: Dict(String, domain.VoiceSettings),
    client: Option(ClientHandle),
    relay_status: String,
    drawer: Option(domain.Drawer),
    now_ms: Int,
    // per-room
    rooms: Dict(String, RoomState),
  )
}
```

Bootstrap flow:
1. `IdentityReady(seed)` → `sunset.create_client(seed)` (no room name).
2. `ClientReady(client)` → for each name in `model.joined_rooms`, dispatch `OpenRoom(name)`. Subscribe relay status. Wire `add_relay` against URL `?relay=` or defaults.
3. `OpenRoom(name)` → effect calls `sunset.open_room(client, name, callback)`. Callback dispatches `RoomOpened(name, handle)`.
4. `RoomOpened(name, handle)` → insert empty `RoomState`; register per-room callbacks; start presence.

Per-room messages:
- `IncomingMsg(name, im)` — append to `model.rooms[name].messages`.
- `IncomingReceipt(name, ...)`, `MembersUpdated(name, ms)` — same shape; first arg is the room name.
- `SubmitDraft` — looks up `model.rooms[active_room].handle`, calls `sunset.send_message(handle, body, ts)`.
- All "open detail / react / select / sheet" messages take an implicit "active room" target (they originate from rendering the active room), so they apply to `model.view = RoomView(name)`.

`JoinRoom(name)`:
- Existing URL/rail bookkeeping unchanged.
- If `model.rooms` does not contain `name`, dispatch `OpenRoom(name)` (the OpenRoom handler re-checks idempotency on the Rust side too).

`DeleteRoom(name)`:
- Call `handle.close()` via FFI (or simply drop the JS-side reference).
- Remove `name` from `model.rooms`.
- Existing list/view bookkeeping unchanged.

### Argon2id cost on page load — stagger

`Room::open` runs Argon2id with production parameters, which takes tens to hundreds of milliseconds on the JS main thread. With N joined rooms opening at startup, that's N × cost serialized.

Mitigation: open the active room first (the one matching `model.view`); then yield to the event loop and open the rest one at a time via `setTimeout(0, ...)` between calls. Active room is interactive immediately; others fill in over the next few seconds. UI shows a per-room loading state until that room's `RoomOpened` arrives.

Persistent caching of `K_room` / `K_epoch_0` / `room_fingerprint` is the right long-term answer (room secrets are stable per name) but is out of scope for this PR.

## Edge cases

- **Opening a room twice** — `Peer::open_room(name)` for an already-open room returns another `OpenRoom` referencing the same `RoomState` (looked up by `RoomFingerprint`). Idempotent. `JoinRoom(name)` for an already-joined room must not stand up a second engine task.
- **Closing while messages in flight** — outstanding `send_text` store inserts aren't cancelled (they're store-level operations, not tied to the handle). The decode task ends, so the user won't see the echo, but the message still ends up in the store. Acceptable.
- **Subscription renewal failure** — log via `tracing` and retry on the next tick; don't tear down the room. The next `add_relay` reconnect will republish.
- **`open_room` failure** — `Room::open` can only fail on Argon2 internal errors (extremely unlikely in practice). UI shows an error banner for that room name; user retries by re-clicking.
- **Page reload race** — `Replay::All` re-emits all entries for every open room on subscribe. The acked-set in each room's decode task starts empty after reload, so we'd auto-ack everything again. Same behavior as today (single-room) — known v1 wart, not made worse by multi-room.
- **WebRTC signaler dispatcher race** — incoming signaling for a room that hasn't been opened yet (peer is ahead of us): drop the entry silently. The entry is still in the store; when we open the room, `Replay::All` picks it up. Same lazy-fetch shape the rest of the system uses.

## Testing

- **`sunset-core` unit tests for `Peer` and `OpenRoom`.** Use `MemoryStore` + a fake `Transport` + a synchronous `Spawner` (a tokio `LocalSet` test-only impl). Cover: open two rooms, send-in-A doesn't appear in B's stream, drop room → subscription/decode-task end, reopen retrieves cached entries.
- **`sunset-core` integration test with two `Peer` instances over an in-memory transport.** Two peers, both open rooms A and B, verify isolation: `peer1` send in A only reaches `peer2` if `peer2` has opened A. Use `AcceptAllVerifier` per the existing convention for sync-internal stub-signed entries.
- **Wasm-bridge Playwright test** in `web/e2e/` exercising room switching: open the page, join room X, send a message, switch to room Y, verify X's message does not appear in Y's stream, send in Y, switch back, verify both rooms' history.
- **Existing single-room e2e tests** — adapt `presence.spec.js` and `kill_relay.spec.js` to call `roomHandle.connect_direct` instead of `client.connect_direct`. No semantic change.

## File layout summary

New / moved in `sunset-core`:
- `crates/sunset-core/src/spawner.rs` (new) — `Spawner` trait
- `crates/sunset-core/src/peer/mod.rs` (new) — `Peer`, `OpenRoom`, `RoomState`
- `crates/sunset-core/src/signaling/mod.rs` (new) — `RelaySignaler` (moved from wasm crate), `MultiRoomSignaler` (new)
- `crates/sunset-core/src/membership.rs` — adds `spawn_publisher` (moved from wasm crate)
- `crates/sunset-core/src/lib.rs` — re-exports

Deleted from `sunset-web-wasm`:
- `crates/sunset-web-wasm/src/relay_signaler.rs` (moved to core)
- `crates/sunset-web-wasm/src/presence_publisher.rs` (moved to core)

Modified in `sunset-web-wasm`:
- `crates/sunset-web-wasm/src/client.rs` — slimmed to thin `Peer` veneer
- `crates/sunset-web-wasm/src/lib.rs` (new module: `room_handle.rs`, `spawner.rs`)
- `crates/sunset-web-wasm/src/spawner.rs` (new) — `WasmSpawner`
- `crates/sunset-web-wasm/src/room_handle.rs` (new) — `RoomHandle` wasm-bindgen veneer

Modified in the Gleam web client:
- `web/src/sunset_web.gleam` — `Model`, `Msg`, `update`, `view` for per-room state and bootstrap flow
- `web/src/sunset_web/sunset.gleam` — FFI signatures for `create_client` (no room arg), new `open_room`, per-room callback registrations
- `web/src/sunset_web/sunset.ffi.mjs` — matching JS shims
- `web/e2e/*.spec.js` — `client.connect_direct` → `roomHandle.connect_direct` rename in two tests; new room-switching test
