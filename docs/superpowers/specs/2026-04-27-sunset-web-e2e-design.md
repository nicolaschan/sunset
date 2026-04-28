# sunset-web end-to-end (Plan E) — Subsystem design

- **Date:** 2026-04-27
- **Status:** Draft (subsystem-level)
- **Scope:** The v0 web demo. Wires the Gleam UI in `web/` to a new `sunset-web-wasm` bundle (sunset-core + sunset-store-memory + sunset-sync + sunset-noise + sunset-sync-ws-browser) so two browsers connected to a deployed `sunset-relay` can exchange end-to-end-encrypted, perfectly-forward-secret-per-epoch, Ed25519-authenticated messages. Plan E in the web roadmap (A → C → D → E.transport → **E**).

## Non-negotiable goals

1. **Two browsers chat across a relay, end-to-end.** A Playwright multi-browser test opens two instances of the Gleam app (each with `?relay=ws://...#x25519=...` pointing at the same `sunset-relay`). Browser A types a message; browser B sees it. The test is the headline acceptance criterion — it proves the entire stack works as a coherent system.
2. **Identity persists across reload.** A 32-byte Ed25519 secret seed is stored in `localStorage` so reloading the page keeps the same identity. (Trade: vulnerable to JS-side XSS; acceptable for v0.)
3. **Crypto stays where it should.** All compose / decode / signing / Noise handshaking happens in the WASM bundle; Gleam never sees plaintext keys. The Gleam side passes the body string in and receives a decoded message out.
4. **The Gleam app continues to work without a relay.** With no `?relay` query param, the app loads, generates/loads identity, shows a "relay not configured" connection-status indicator, but doesn't crash. Critical so the Gleam dev workflow (running `nix run .#web-dev` without a relay) keeps functioning.

## Non-goals (deferred)

- **IndexedDB persistence.** Per the user's earlier decision, v0 uses `MemoryStore` in the browser; the relay holds the durable copy. On reload the browser re-syncs from the relay.
- **Multi-room UI.** The Gleam UI's room list shows multiple rooms but selecting a different one is a no-op for v0. Single hardcoded room name `"sunset-demo"`. Multi-room is a follow-up.
- **Multi-relay redundancy on the client side.** The Plan D relay-as-peer federation handles redundancy server-side; the client connects to one URL.
- **Relay URL UI** (config dialog, settings page). v0 reads the URL from `?relay=` only. Operators communicate the relay URL out-of-band.
- **Identity rotation / "remember me" toggle / sign-out.** Identity sticks to localStorage forever in v0; there's no UI to clear it.
- **Read receipts, edits, deletes, member list, presence, voice.** All sunset-core op types beyond "post a message" are deferred to later plans.
- **Mobile-specific UI work.** Frontend Claude is brainstorming a mobile design; that's a separate plan and lives in `web/` independently of this plan's WASM wiring.

## Architecture

```
Gleam UI (Lustre) — web/src/sunset_web/
   │
   │ via web/src/sunset_web/sunset.gleam externals
   ▼
JS shim — web/src/sunset_web/sunset.ffi.mjs
   │
   │ wasm-bindgen-loaded JS module
   ▼
JS Client class (exported by sunset-web-wasm)
   │
   │ holds + drives
   ▼
SyncEngine<MemoryStore, NoiseTransport<WebSocketRawTransport>>
   │
   │ over WebSocket+Noise
   ▼
sunset-relay (deployed; or localhost during Playwright)
```

Key boundaries:

- **Gleam ↔ JS shim**: typed Gleam externals; the shim exposes a small JS API.
- **JS shim ↔ wasm Client**: wasm-bindgen handles marshaling. The shim's only job is to instantiate the wasm module and forward calls.
- **Wasm Client ↔ SyncEngine**: idiomatic Rust; the engine owns the store and the transport.
- **SyncEngine ↔ Relay**: real WebSocket + Noise (Plans C/E.transport).

### Wasm bundle: `crates/sunset-web-wasm`

A new crate that mirrors `crates/sunset-core-wasm` (Plan A) but adds the SyncEngine machinery. Exposes one stateful JS class instead of pure functions.

```
crates/sunset-web-wasm/
├── Cargo.toml
└── src/
    ├── lib.rs              # crate-level boilerplate + re-exports
    ├── client.rs           # the JS-exported Client class
    ├── identity.rs         # Identity persistence helpers (seed in/out of bytes)
    └── messages.rs         # IncomingMessage struct + on_message callback plumbing
```

#### Client JS surface

```rust
#[wasm_bindgen]
pub struct Client { /* private */ }

#[wasm_bindgen]
impl Client {
    /// Construct from a 32-byte secret seed (the JS side reads/writes this
    /// to localStorage) and a room name.
    ///
    /// Opens the in-memory store, builds the SyncEngine, registers the
    /// caller-supplied `on_message` callback. Does NOT connect to any
    /// relay — call `add_relay()` for that.
    #[wasm_bindgen(constructor)]
    pub fn new(
        seed: &[u8],            // 32 bytes; throws if not
        room_name: &str,
    ) -> Result<Client, JsError>;

    /// The Ed25519 public key of this client's identity.
    #[wasm_bindgen(getter)]
    pub fn public_key(&self) -> Vec<u8>;

    /// Open a connection to the relay. PeerAddr in the
    /// `ws://host:port#x25519=hex` form.
    pub async fn add_relay(&self, url_with_fragment: &str) -> Result<(), JsError>;

    /// Subscribe the engine to "all messages in this room". Idempotent —
    /// safe to call after every relay connect.
    pub async fn publish_room_subscription(&self) -> Result<(), JsError>;

    /// Compose a message in this room and write it to the local store
    /// (sync pushes it to the relay automatically). Returns the entry's
    /// value-hash hex so the caller can echo "pending → confirmed".
    pub async fn send_message(
        &self,
        body: &str,
        sent_at_ms: f64,        // JS Number; cast to u64
        nonce_seed: &[u8],      // 32 bytes from crypto.getRandomValues
    ) -> Result<String, JsError>;

    /// Register the callback invoked once per inbound message (own or
    /// received). The callback is `(IncomingMessage) => void`.
    /// Called for every message currently in the local store + every
    /// message that arrives later. Replays history on registration.
    pub fn on_message(&self, callback: js_sys::Function);

    /// Connection status (for the Gleam UI's badge).
    #[wasm_bindgen(getter)]
    pub fn relay_status(&self) -> String;  // "disconnected" | "connecting" | "connected" | "error"
}

#[wasm_bindgen]
pub struct IncomingMessage {
    #[wasm_bindgen(getter_with_clone)] pub author_pubkey: Vec<u8>,   // 32 bytes
    pub epoch_id: u64,
    pub sent_at_ms: f64,
    #[wasm_bindgen(getter_with_clone)] pub body: String,
    #[wasm_bindgen(getter_with_clone)] pub value_hash_hex: String,
    pub is_self: bool,
}
```

#### Engine event loop in the browser

`engine.run()` is `async`. The Client's constructor calls `wasm_bindgen_futures::spawn_local(engine.run())` to drive it on the browser's microtask queue. The engine runs until the Client is dropped.

The `on_message` callback is invoked from the browser's microtask queue too — not from a real thread (wasm is single-threaded), so there's no concurrency concern. The callback is held in a `RefCell<Option<js_sys::Function>>` on the Client; the engine's own subscription task pushes decoded messages through it.

#### Identity persistence

`web/src/sunset_web/sunset.ffi.mjs` reads/writes a hex string at localStorage key `sunset/identity-seed`. On first load: generate via `crypto.getRandomValues(new Uint8Array(32))`, persist, pass to `new Client(seed, room)`. On subsequent loads: read from localStorage, decode, pass.

This is a JS-side concern. The wasm crate doesn't touch localStorage.

### Gleam side wiring

#### New files

```
web/src/sunset_web/
├── sunset.gleam                  # NEW: Gleam externals over the wasm Client API
└── sunset.ffi.mjs                # NEW: JS shim that loads the wasm module + exposes a typed JS interface
```

#### Modified files

`web/src/sunset_web.gleam` — replace fixture-driven model with real-data model:

- Identity + room init at app start (effect that calls into the shim, gets back the public key + relay URL).
- Relay connect on app start if `?relay=...` present.
- Replace `fixture.messages()` reads with a model field `messages: List(Message)` populated from `on_message` callbacks (delivered as Lustre messages).
- Composer submit calls into the shim's `send_message`; the optimistic UI shows the message as `pending: True` immediately, then flips to `pending: False` when the engine's own subscription delivers it back via `on_message` (matching by `value_hash_hex`).

`web/src/sunset_web/storage.ffi.mjs` — add localStorage helpers for `sunset/identity-seed` (mirror existing `joined-rooms` pattern).

#### Wiring sketch

```gleam
// sunset.gleam
@external(javascript, "./sunset.ffi.mjs", "load_or_create_identity")
pub fn load_or_create_identity() -> Promise(BitArray)

@external(javascript, "./sunset.ffi.mjs", "create_client")
pub fn create_client(seed: BitArray, room_name: String) -> Promise(ClientHandle)

@external(javascript, "./sunset.ffi.mjs", "client_add_relay")
pub fn client_add_relay(client: ClientHandle, url: String) -> Promise(Result(Nil, String))

@external(javascript, "./sunset.ffi.mjs", "client_send_message")
pub fn client_send_message(
  client: ClientHandle,
  body: String,
  sent_at_ms: Int,
) -> Promise(Result(String, String))   // returns the value_hash_hex

@external(javascript, "./sunset.ffi.mjs", "client_on_message")
pub fn client_on_message(
  client: ClientHandle,
  callback: fn(IncomingMessage) -> Nil,
) -> Nil

pub type ClientHandle    // opaque
pub type IncomingMessage {
  IncomingMessage(
    author_pubkey: BitArray,
    epoch_id: Int,
    sent_at_ms: Int,
    body: String,
    value_hash_hex: String,
    is_self: Bool,
  )
}

/// Read `?relay=<url-encoded>` from the current URL. None if absent.
@external(javascript, "./sunset.ffi.mjs", "relay_url_param")
pub fn relay_url_param() -> Result(String, Nil)
```

#### Update flow

```
App init (Lustre):
  1. effect: load_or_create_identity() → seed_bytes
  2. effect: create_client(seed_bytes, "sunset-demo") → client_handle
  3. effect: register on_message callback that dispatches `Msg::IncomingMessage(...)` into Lustre's update loop
  4. effect: relay_url_param() → if Some(url), client_add_relay(client_handle, url) + client_publish_room_subscription
  5. UI renders normally

User submits a message:
  1. effect: client_send_message(handle, body, now_ms) → value_hash_hex
  2. UI optimistic: append Message { id: value_hash_hex, body, you: true, pending: true } to model.messages
  3. Engine round-trip: on_message fires for the same value_hash_hex
  4. UI: find the matching message by value_hash_hex, flip pending: false

Inbound message:
  1. on_message fires with IncomingMessage from a remote author
  2. UI: append Message { id: value_hash_hex, body, you: is_self, pending: false, ... } to model.messages
```

### Build pipeline

`flake.nix` adds:

```nix
packages.sunset-web-wasm = pkgs.rustPlatform.buildRustPackage {
  # Same recipe as packages.sunset-core-wasm from Plan A, but for sunset-web-wasm.
  ...
};
```

`web/`'s build (the existing `gleamLib.buildGleamPackage` derivation in flake.nix) is extended to also copy `sunset-web-wasm`'s output (`.js` glue + `.wasm`) into the dist tree. Two ways:

- **(a) Symlink-copy at install time** in `webDist`'s `installPhase`. Adds `cp ${packages.sunset-web-wasm}/* $out/`. Simple. The Gleam app's `import` of `./sunset_web_wasm.js` (relative to the served root) just works.
- **(b) Vite/esbuild bundling step.** Heavier; brings in JS bundling. Out of scope for v0.

(a) it is. The output is just two files (`.js` + `.wasm`), serving them directly is fine.

### Default relay URL fallback

If `?relay` query param is absent, the wasm Client is constructed normally but `client_add_relay` is not called. The UI shows the existing `ConnStatus::Offline` badge (already part of the domain model — `Member.relay: NoRelay` and similar). A small banner "Relay not configured — pass `?relay=ws://...#x25519=...` to connect" is rendered.

A slightly nicer fallback: if the page is served from `localhost`, default to `ws://localhost:8443#x25519=<hex>` reading the hex from a small `localhost-relay.json` next to `index.html`. Nice for `nix run .#web-dev` workflows. But this adds complexity; defer to a follow-up.

For v0: no `?relay` = no connection. Playwright tests always supply `?relay`.

### Playwright e2e test

The headline test, lives in `web/e2e/two_browser_chat.spec.js` (or wherever Playwright already looks — frontend Claude has set this up).

```text
1. Spin up sunset-relay subprocess on a random port (use node's child_process or
   a Playwright globalSetup hook).
2. Capture the relay's address line from stdout (`address: ws://0.0.0.0:NNNN#x25519=HHH`).
3. URL-encode it for use as `?relay=...`.
4. Open browser context A; goto `https://localhost:5173/?relay=<encoded>`.
5. Wait for connection-status badge to read "connected" (or equivalent).
6. Open browser context B; same URL (different identity since localStorage is per-context).
7. In context A: type "hello from A" in the composer, click submit.
8. In context B: assert the message "hello from A" appears in the messages list.
9. (And reverse: B sends, A receives.)
10. Teardown: kill the relay subprocess.
```

The test runs both real browsers (or one real + one headless), real Gleam app, real wasm bundle, real WebSocket+Noise, real relay. End-to-end at every layer.

## Tests + verification

- **Native build**: `cargo build -p sunset-web-wasm` (uses native stub paths from sunset-sync-ws-browser; doesn't actually connect to anything).
- **Wasm build**: `cargo build -p sunset-web-wasm --target wasm32-unknown-unknown` clean.
- **wasm-bindgen-test**: a single `#[wasm_bindgen_test(run_in_node_experimental)]` constructs a Client + verifies the JS surface compiles. (No actual networking — Node WebSocket polyfill story is uneven; real e2e is the Playwright test.)
- **Workspace tests**: `cargo test --workspace --all-features` green; nothing regresses.
- **Workspace clippy**: `cargo clippy --workspace --all-features --all-targets -- -D warnings` clean.
- **All prior nix builds** (`sunset-core-wasm`, `sunset-relay`, `sunset-relay-docker`) still produce artifacts.
- **`nix build .#sunset-web-wasm`** produces `result/sunset_web_wasm.js` + `result/sunset_web_wasm_bg.wasm`.
- **`nix build .#web`** still succeeds and now includes the wasm artifacts.
- **Gleam build**: `gleam build` in `web/` succeeds; the new `sunset.gleam` externals resolve.
- **Playwright e2e**: `nix run .#web-test -- two-browser-chat.spec.js` (or the equivalent established invocation) — passes; both browsers see each other's messages.

## Items deferred

- IndexedDB persistence (`sunset-store-indexeddb` crate).
- Multi-room UI; multi-relay client-side redundancy.
- Identity rotation / sign-out / device management.
- Read receipts, edits, deletes, member lists, presence, voice.
- Connection retry with backoff (when WebSocket drops, the client just shows "disconnected" — user reload reconnects).
- Mobile-specific UI work.
- Bundle-size optimization (wasm-opt, code splitting).

## Self-review checklist

- [x] Four non-negotiables (two browsers chat, identity persists, crypto stays in wasm, no-relay mode works) are met by named mechanisms.
- [x] Crate split rationale (sunset-web-wasm separate from sunset-core-wasm) is explicit.
- [x] Identity persistence path (localStorage) is concrete + risks acknowledged.
- [x] Relay URL config (`?relay=` query param, URL-encoded) is concrete + collision with room-name fragment is addressed.
- [x] Engine event loop (spawn_local) and callback delivery (RefCell<Option<Function>>) are addressed.
- [x] Default-no-relay behavior is specified (don't crash, show disconnected indicator).
- [x] Playwright test scenario is concrete enough to plan against.
- [x] Out-of-scope items prevent scope creep.
