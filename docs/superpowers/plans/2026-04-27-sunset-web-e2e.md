# sunset-web end-to-end (Plan E) — Implementation Plan

> **For agentic workers:** Use superpowers:executing-plans (or superpowers:subagent-driven-development) to execute this plan task-by-task.

**Goal:** Land Plan E. After this plan: two browsers connected to a deployed `sunset-relay` exchange end-to-end-encrypted messages via the existing Gleam UI in `web/`. The headline acceptance test is a Playwright multi-browser scenario.

**Spec:** `docs/superpowers/specs/2026-04-27-sunset-web-e2e-design.md`.

**Out of scope (per spec):** IndexedDB persistence, multi-room UI, identity rotation/sign-out, read receipts/edits/deletes/voice, multi-relay client redundancy, mobile-specific UI work, connection retry with backoff.

---

## File structure

```
sunset/
├── Cargo.toml                                  # MODIFY: workspace add sunset-web-wasm member
├── flake.nix                                   # MODIFY: add packages.sunset-web-wasm; extend webDist installPhase to copy wasm artifacts
├── crates/
│   └── sunset-web-wasm/                        # NEW
│       ├── Cargo.toml
│       └── src/
│           ├── lib.rs                          # crate-level + re-exports
│           ├── client.rs                       # JS-exported Client class
│           ├── identity.rs                     # seed → Identity helper
│           └── messages.rs                     # IncomingMessage + on_message plumbing
├── crates/sunset-web-wasm/tests/
│   └── construct.rs                            # wasm-bindgen-test
├── web/src/sunset_web/
│   ├── sunset.gleam                            # NEW: Gleam externals
│   └── sunset.ffi.mjs                          # NEW: JS shim that loads wasm
├── web/src/sunset_web.gleam                    # MODIFY: replace fixture data with real engine
├── web/src/sunset_web/storage.ffi.mjs          # MODIFY: add identity-seed localStorage helpers
└── web/e2e/two_browser_chat.spec.js            # NEW: Playwright headline test
```

(Frontend Claude has Playwright wired in; the new spec slots into their existing `web/e2e/` setup. Read `web/playwright.config.js` to confirm the path convention.)

---

## Tasks

### Task 1: Scaffold `sunset-web-wasm` crate

**Files:**
- Modify: `Cargo.toml` (workspace root)
- Create: `crates/sunset-web-wasm/Cargo.toml`
- Create: `crates/sunset-web-wasm/src/{lib,client,identity,messages}.rs` (placeholders)

- [ ] **Step 1:** Add `crates/sunset-web-wasm` to root `[workspace] members`. No new workspace deps needed (everything we need was added by Plans A/C/E.transport).

- [ ] **Step 2:** Create `crates/sunset-web-wasm/Cargo.toml`:

  ```toml
  [package]
  name = "sunset-web-wasm"
  version.workspace = true
  edition.workspace = true
  license.workspace = true
  rust-version.workspace = true

  [lib]
  crate-type = ["cdylib", "rlib"]

  [lints]
  workspace = true

  [dependencies]
  bytes.workspace = true
  futures = { workspace = true, default-features = false, features = ["std", "alloc"] }
  hex.workspace = true
  rand_chacha.workspace = true
  rand_core.workspace = true
  sunset-core.workspace = true
  sunset-noise.workspace = true
  sunset-store.workspace = true
  sunset-store-memory.workspace = true
  sunset-sync.workspace = true
  sunset-sync-ws-browser.workspace = true
  thiserror.workspace = true
  zeroize.workspace = true

  [target.'cfg(target_arch = "wasm32")'.dependencies]
  js-sys.workspace = true
  wasm-bindgen.workspace = true
  wasm-bindgen-futures.workspace = true

  [target.'cfg(target_arch = "wasm32")'.dev-dependencies]
  wasm-bindgen-test.workspace = true
  ```

  Add `sunset-sync-ws-browser = { path = "crates/sunset-sync-ws-browser" }` to root `[workspace.dependencies]` if it's not already there (Plan E.transport may have skipped it since no other crate consumed it at the time).

- [ ] **Step 3:** Create `crates/sunset-web-wasm/src/lib.rs`:

  ```rust
  //! WASM bundle: sunset-core + sunset-store-memory + sunset-sync +
  //! sunset-noise + sunset-sync-ws-browser, exposed to JS as a `Client` class.
  //!
  //! See `docs/superpowers/specs/2026-04-27-sunset-web-e2e-design.md`.

  #[cfg(target_arch = "wasm32")]
  mod wasm {
      pub mod client;
      pub mod identity;
      pub mod messages;
  }

  #[cfg(target_arch = "wasm32")]
  pub use wasm::client::Client;
  #[cfg(target_arch = "wasm32")]
  pub use wasm::messages::IncomingMessage;

  #[cfg(not(target_arch = "wasm32"))]
  pub struct Client;
  #[cfg(not(target_arch = "wasm32"))]
  pub struct IncomingMessage;
  ```

- [ ] **Step 4:** Create `crates/sunset-web-wasm/src/client.rs`, `identity.rs`, `messages.rs` — each with `//! Placeholder; populated in a later task of this plan.`

  Adjust the directory layout to match `mod wasm { pub mod client; ... }` form: actually create them at `crates/sunset-web-wasm/src/wasm/{client,identity,messages}.rs` and a `crates/sunset-web-wasm/src/wasm/mod.rs` (or use the `#[path = "..."]` attribute). Cleanest: just have `src/{client,identity,messages}.rs` and adjust the lib.rs to reference them at the top level cfg-gated:

  ```rust
  #[cfg(target_arch = "wasm32")]
  mod client;
  #[cfg(target_arch = "wasm32")]
  mod identity;
  #[cfg(target_arch = "wasm32")]
  mod messages;

  #[cfg(target_arch = "wasm32")]
  pub use client::Client;
  #[cfg(target_arch = "wasm32")]
  pub use messages::IncomingMessage;

  #[cfg(not(target_arch = "wasm32"))]
  pub struct Client;
  #[cfg(not(target_arch = "wasm32"))]
  pub struct IncomingMessage;
  ```

  Use this simpler form. Three files at top level.

- [ ] **Step 5:** Verify both build paths:
  ```
  nix develop --command cargo fmt -p sunset-web-wasm
  nix develop --command cargo build -p sunset-web-wasm
  nix develop --command cargo build -p sunset-web-wasm --target wasm32-unknown-unknown
  ```

- [ ] **Step 6:** Commit:
  ```
  git -C /home/nicolas/src/sunset/.worktrees/sunset-web-e2e add Cargo.toml Cargo.lock crates/sunset-web-wasm/
  git -C /home/nicolas/src/sunset/.worktrees/sunset-web-e2e commit -m "Scaffold sunset-web-wasm crate (cfg-gated wasm bundle)"
  ```

---

### Task 2: Identity helper + IncomingMessage type

**Files:**
- Modify: `crates/sunset-web-wasm/src/identity.rs`
- Modify: `crates/sunset-web-wasm/src/messages.rs`

- [ ] **Step 1:** Replace `crates/sunset-web-wasm/src/identity.rs`:

  ```rust
  //! Helpers for constructing sunset-core Identity from a JS-supplied seed.

  use sunset_core::Identity;

  /// Build an Identity from a 32-byte secret seed.
  pub fn identity_from_seed(seed: &[u8]) -> Result<Identity, String> {
      let arr: [u8; 32] = seed
          .try_into()
          .map_err(|_| format!("identity seed must be 32 bytes, got {}", seed.len()))?;
      Ok(Identity::from_secret_bytes(&arr))
  }
  ```

- [ ] **Step 2:** Replace `crates/sunset-web-wasm/src/messages.rs`:

  ```rust
  //! IncomingMessage type exposed to JS + helpers to convert from sunset-core's
  //! DecodedMessage.

  use wasm_bindgen::prelude::*;

  use sunset_core::DecodedMessage;

  /// JS-facing decoded message. Mirrors sunset-core's DecodedMessage but
  /// uses JS-friendly types (BigInt → f64 for timestamps, Vec<u8> → Uint8Array).
  #[wasm_bindgen]
  pub struct IncomingMessage {
      #[wasm_bindgen(getter_with_clone)]
      pub author_pubkey: Vec<u8>,
      pub epoch_id: u64,
      pub sent_at_ms: f64,
      #[wasm_bindgen(getter_with_clone)]
      pub body: String,
      #[wasm_bindgen(getter_with_clone)]
      pub value_hash_hex: String,
      pub is_self: bool,
  }

  pub fn from_decoded(
      decoded: DecodedMessage,
      value_hash_hex: String,
      is_self: bool,
  ) -> IncomingMessage {
      IncomingMessage {
          author_pubkey: decoded.author_key.as_bytes().to_vec(),
          epoch_id: decoded.epoch_id,
          sent_at_ms: decoded.sent_at_ms as f64,
          body: decoded.body,
          value_hash_hex,
          is_self,
      }
  }
  ```

- [ ] **Step 3:** Verify:
  ```
  nix develop --command cargo build -p sunset-web-wasm --target wasm32-unknown-unknown
  ```

- [ ] **Step 4:** Commit:
  ```
  git add crates/sunset-web-wasm/src/identity.rs crates/sunset-web-wasm/src/messages.rs
  git commit -m "Add Identity helper + IncomingMessage type for the JS bridge"
  ```

---

### Task 3: `Client` struct + constructor + engine spawn

**Files:**
- Modify: `crates/sunset-web-wasm/src/client.rs`

This task wires up the Client constructor: build the Identity, the Room, the MemoryStore, the WebSocketRawTransport, the NoiseTransport, the SyncEngine, then spawn the engine's `run` on the browser microtask queue.

The `add_relay`, `send_message`, `on_message` methods are stubs in this task; they get filled in by later tasks. This keeps the commit boundaries clean.

- [ ] **Step 1:** Replace `crates/sunset-web-wasm/src/client.rs`:

  ```rust
  //! JS-exported Client: identity + room + sync engine wired together.

  use std::cell::RefCell;
  use std::rc::Rc;
  use std::sync::Arc;

  use bytes::Bytes;
  use wasm_bindgen::prelude::*;
  use zeroize::Zeroizing;

  use sunset_core::{Ed25519Verifier, Identity, Room};
  use sunset_noise::{NoiseIdentity, NoiseTransport};
  use sunset_store::VerifyingKey;
  use sunset_store_memory::MemoryStore;
  use sunset_sync::{PeerId, Signer, SyncConfig, SyncEngine};
  use sunset_sync_ws_browser::WebSocketRawTransport;

  use crate::identity::identity_from_seed;
  use crate::messages::IncomingMessage;

  type Engine = SyncEngine<MemoryStore, NoiseTransport<WebSocketRawTransport>>;

  /// Adapter so sunset-core's `Identity` works as a NoiseIdentity.
  /// (Same pattern as Plan C/D's integration tests; lives here for now.)
  struct IdentityNoiseAdapter(Identity);

  impl NoiseIdentity for IdentityNoiseAdapter {
      fn ed25519_public(&self) -> [u8; 32] {
          self.0.public().as_bytes()
      }
      fn ed25519_secret_seed(&self) -> Zeroizing<[u8; 32]> {
          Zeroizing::new(self.0.secret_bytes())
      }
  }

  #[wasm_bindgen]
  pub struct Client {
      identity: Identity,
      room: Rc<Room>,
      store: Arc<MemoryStore>,
      engine: Rc<Engine>,
      on_message: Rc<RefCell<Option<js_sys::Function>>>,
      relay_status: Rc<RefCell<&'static str>>,
  }

  #[wasm_bindgen]
  impl Client {
      #[wasm_bindgen(constructor)]
      pub fn new(seed: &[u8], room_name: &str) -> Result<Client, JsError> {
          let identity =
              identity_from_seed(seed).map_err(|e| JsError::new(&e))?;
          let room = Rc::new(
              Room::open(room_name)
                  .map_err(|e| JsError::new(&format!("Room::open: {e}")))?,
          );

          let store = Arc::new(MemoryStore::new(Arc::new(Ed25519Verifier)));

          let raw = WebSocketRawTransport::dial_only();
          let noise = NoiseTransport::new(
              raw,
              Arc::new(IdentityNoiseAdapter(identity.clone())),
          );

          let local_peer = PeerId(identity.store_verifying_key());
          let signer: Arc<dyn Signer> = Arc::new(identity.clone());
          let engine = Rc::new(SyncEngine::new(
              store.clone(),
              noise,
              SyncConfig::default(),
              local_peer,
              signer,
          ));

          // Spawn the engine event loop on the browser microtask queue.
          let engine_clone = engine.clone();
          wasm_bindgen_futures::spawn_local(async move {
              if let Err(e) = engine_clone.run().await {
                  web_sys::console::error_1(
                      &JsValue::from_str(&format!("sync engine exited: {e}")),
                  );
              }
          });

          Ok(Client {
              identity,
              room,
              store,
              engine,
              on_message: Rc::new(RefCell::new(None)),
              relay_status: Rc::new(RefCell::new("disconnected")),
          })
      }

      #[wasm_bindgen(getter)]
      pub fn public_key(&self) -> Vec<u8> {
          self.identity.public().as_bytes().to_vec()
      }

      #[wasm_bindgen(getter)]
      pub fn relay_status(&self) -> String {
          (*self.relay_status.borrow()).to_owned()
      }

      // add_relay, publish_room_subscription, send_message, on_message:
      // populated in Tasks 4 + 5 + 6 + 7.
  }
  ```

  Add `web-sys = { ..., features = [..., "console"] }` if it isn't already a dep — needed for `console::error_1`. (sunset-sync-ws-browser added web-sys with WebSocket features; we may need to enable Console feature on our own crate's web-sys dep too. Actually web-sys features are additive across the dep graph, so if our crate doesn't directly use web-sys we may not need to depend on it — try without first.)

- [ ] **Step 2:** Verify:
  ```
  nix develop --command cargo fmt -p sunset-web-wasm
  nix develop --command cargo build -p sunset-web-wasm --target wasm32-unknown-unknown
  ```

  If `web_sys::console` is unresolved, add `web-sys = { workspace = true, features = ["console"] }` to the wasm-target deps in `Cargo.toml`.

- [ ] **Step 3:** Commit:
  ```
  git add crates/sunset-web-wasm/src/client.rs crates/sunset-web-wasm/Cargo.toml
  git commit -m "Add Client constructor: build engine + spawn on microtask queue"
  ```

---

### Task 4: `add_relay` + `publish_room_subscription` methods

**Files:**
- Modify: `crates/sunset-web-wasm/src/client.rs`

- [ ] **Step 1:** Add to the `impl Client` block (after `relay_status` getter):

  ```rust
  pub async fn add_relay(&self, url_with_fragment: String) -> Result<(), JsError> {
      *self.relay_status.borrow_mut() = "connecting";
      let addr = sunset_sync::PeerAddr::new(Bytes::from(url_with_fragment));
      match self.engine.add_peer(addr).await {
          Ok(()) => {
              *self.relay_status.borrow_mut() = "connected";
              Ok(())
          }
          Err(e) => {
              *self.relay_status.borrow_mut() = "error";
              Err(JsError::new(&format!("add_relay: {e}")))
          }
      }
  }

  pub async fn publish_room_subscription(&self) -> Result<(), JsError> {
      use std::time::Duration;
      let filter = sunset_core::room_messages_filter(&self.room);
      self.engine
          .publish_subscription(filter, Duration::from_secs(3600))
          .await
          .map_err(|e| JsError::new(&format!("publish_subscription: {e}")))?;
      Ok(())
  }
  ```

  Note: `add_relay` takes `String` (not `&str`) because wasm-bindgen async methods don't currently support borrowing references. If the implementer finds that signature is incompatible, switch to `&str` and report what the wasm-bindgen error said.

- [ ] **Step 2:** Verify:
  ```
  nix develop --command cargo fmt -p sunset-web-wasm
  nix develop --command cargo build -p sunset-web-wasm --target wasm32-unknown-unknown
  nix develop --command cargo clippy -p sunset-web-wasm --all-targets --target wasm32-unknown-unknown -- -D warnings
  ```

- [ ] **Step 3:** Commit:
  ```
  git add crates/sunset-web-wasm/src/client.rs
  git commit -m "Add Client::add_relay + publish_room_subscription"
  ```

---

### Task 5: `send_message` method

**Files:**
- Modify: `crates/sunset-web-wasm/src/client.rs`

- [ ] **Step 1:** Add the method to `impl Client`:

  ```rust
  pub async fn send_message(
      &self,
      body: String,
      sent_at_ms: f64,
      nonce_seed: Vec<u8>,
  ) -> Result<String, JsError> {
      let nonce_seed_arr: [u8; 32] = nonce_seed
          .as_slice()
          .try_into()
          .map_err(|_| JsError::new("nonce_seed must be 32 bytes"))?;

      let mut rng = rand_chacha::ChaCha20Rng::from_seed(nonce_seed_arr);
      use sunset_store::Store as _;

      let composed = sunset_core::compose_message(
          &self.identity,
          &self.room,
          0u64,
          sent_at_ms as u64,
          &body,
          &mut rng,
      )
      .map_err(|e| JsError::new(&format!("compose_message: {e}")))?;

      let value_hash_hex = composed.entry.value_hash.to_hex();

      self.store
          .insert(composed.entry, Some(composed.block))
          .await
          .map_err(|e| JsError::new(&format!("store insert: {e}")))?;

      Ok(value_hash_hex)
  }
  ```

  Add `use rand_core::SeedableRng;` near the top of the file (needed for `ChaCha20Rng::from_seed`).

- [ ] **Step 2:** Verify the wasm build + clippy:
  ```
  nix develop --command cargo fmt -p sunset-web-wasm
  nix develop --command cargo build -p sunset-web-wasm --target wasm32-unknown-unknown
  nix develop --command cargo clippy -p sunset-web-wasm --all-targets --target wasm32-unknown-unknown -- -D warnings
  ```

- [ ] **Step 3:** Commit:
  ```
  git add crates/sunset-web-wasm/src/client.rs
  git commit -m "Add Client::send_message: compose + insert into local store"
  ```

---

### Task 6: `on_message` callback registration + delivery

**Files:**
- Modify: `crates/sunset-web-wasm/src/client.rs`

This is the most subtle part of the bundle. The Client subscribes to its own store (with `Replay::All` so existing messages are emitted on registration too); the subscription stream runs on a `spawn_local` task; for each event it filters to "message inserted" entries, decodes them via `sunset_core::decode_message`, and invokes the JS callback.

- [ ] **Step 1:** Add to `impl Client`:

  ```rust
  pub fn on_message(&self, callback: js_sys::Function) {
      *self.on_message.borrow_mut() = Some(callback);
      self.spawn_message_subscription();
  }

  fn spawn_message_subscription(&self) {
      let store = self.store.clone();
      let room = self.room.clone();
      let identity_pub = self.identity.public();
      let on_message = self.on_message.clone();

      wasm_bindgen_futures::spawn_local(async move {
          use futures::StreamExt;
          use sunset_core::{decode_message, room_messages_filter};
          use sunset_store::{Event, Replay, Store as _};

          let filter = room_messages_filter(&room);
          let mut events = match store.subscribe(filter, Replay::All).await {
              Ok(s) => s,
              Err(e) => {
                  web_sys::console::error_1(&JsValue::from_str(&format!(
                      "store.subscribe: {e}"
                  )));
                  return;
              }
          };

          while let Some(ev) = events.next().await {
              let entry = match ev {
                  Ok(Event::Inserted(e)) => e,
                  Ok(Event::Replaced { new, .. }) => new,
                  Ok(_) => continue,
                  Err(e) => {
                      web_sys::console::error_1(&JsValue::from_str(&format!(
                          "store event: {e}"
                      )));
                      continue;
                  }
              };

              let block = match store.get_content(&entry.value_hash).await {
                  Ok(Some(b)) => b,
                  Ok(None) => continue,        // blob not arrived yet; sync will deliver
                  Err(e) => {
                      web_sys::console::error_1(&JsValue::from_str(&format!(
                          "get_content: {e}"
                      )));
                      continue;
                  }
              };

              let decoded = match decode_message(&room, &entry, &block) {
                  Ok(d) => d,
                  Err(e) => {
                      web_sys::console::error_1(&JsValue::from_str(&format!(
                          "decode_message: {e}"
                      )));
                      continue;
                  }
              };

              let is_self = decoded.author_key == identity_pub;
              let value_hash_hex = entry.value_hash.to_hex();
              let incoming = crate::messages::from_decoded(decoded, value_hash_hex, is_self);

              if let Some(cb) = on_message.borrow().as_ref() {
                  let _ = cb.call1(&JsValue::NULL, &JsValue::from(incoming));
              }
          }
      });
  }
  ```

  Two things to flag:

  - The closure captures `store: Arc<MemoryStore>` and the engine subscription stream. The store is `Arc`, the engine is `Rc<Engine>` — fine in single-threaded wasm.
  - The `decode_message` step fails if a content block hasn't arrived yet (the entry arrives slightly before its referenced blob in some cases). The `Ok(None)` branch silently skips; in v0 this is acceptable because sync re-delivers via a later `Replaced` event when the blob lands. (A more robust impl would re-queue; defer.)

- [ ] **Step 2:** Verify:
  ```
  nix develop --command cargo fmt -p sunset-web-wasm
  nix develop --command cargo build -p sunset-web-wasm --target wasm32-unknown-unknown
  nix develop --command cargo clippy -p sunset-web-wasm --all-targets --target wasm32-unknown-unknown -- -D warnings
  ```

- [ ] **Step 3:** Commit:
  ```
  git add crates/sunset-web-wasm/src/client.rs
  git commit -m "Add Client::on_message: subscribe + decode + JS callback dispatch"
  ```

---

### Task 7: wasm-bindgen-test (compile + construct)

**Files:**
- Create: `crates/sunset-web-wasm/tests/construct.rs`

- [ ] **Step 1:** Create the test:

  ```rust
  //! Compile + construct check for the JS bridge. Real e2e is the Playwright
  //! test in web/e2e/two_browser_chat.spec.js (Task 11).

  #![cfg(target_arch = "wasm32")]

  use sunset_web_wasm::Client;
  use wasm_bindgen_test::*;

  wasm_bindgen_test_configure!(run_in_node_experimental);

  #[wasm_bindgen_test]
  fn client_constructs() {
      let seed = [42u8; 32];
      let client = Client::new(&seed, "plan-e-test").expect("Client::new");
      let pk = client.public_key();
      assert_eq!(pk.len(), 32);
      let status = client.relay_status();
      assert_eq!(status, "disconnected");
  }
  ```

  Note: `Client::new` does Argon2id under the hood (via `Room::open`'s production params). Under wasm-pack node with hot CPU caches this still completes quickly enough for a unit test, but if the test times out, switch to a `Room::open_with_params` variant — would require adding a new `#[wasm_bindgen]` constructor variant. Try the simple form first.

- [ ] **Step 2:** Run wasm-pack:
  ```
  cd /home/nicolas/src/sunset/.worktrees/sunset-web-e2e
  nix develop --command bash -c 'cd crates/sunset-web-wasm && wasm-pack test --node'
  ```

  Expect 1 passed.

- [ ] **Step 3:** Commit:
  ```
  git add crates/sunset-web-wasm/tests/construct.rs
  git commit -m "Add wasm-bindgen-test: Client constructs + exposes public_key"
  ```

---

### Task 8: Flake derivation `packages.sunset-web-wasm` + extend `webDist`

**Files:**
- Modify: `flake.nix`

- [ ] **Step 1:** Read `flake.nix` to find Plan A's `packages.sunset-core-wasm` derivation. Add a parallel `packages.sunset-web-wasm` next to it, using the same recipe (custom buildPhase that runs `cargo build` then `wasm-bindgen`). Substitute `sunset-core-wasm` → `sunset-web-wasm` throughout. Add `pkgs.lld` to `nativeBuildInputs` (Plan A added this).

  ```nix
  sunset-web-wasm = pkgs.rustPlatform.buildRustPackage {
    pname = "sunset-web-wasm";
    version = "0.1.0";
    src = ./.;
    cargoLock.lockFile = ./Cargo.lock;
    doCheck = false;
    nativeBuildInputs = [ pkgs.wasm-bindgen-cli pkgs.lld ];
    cargo = rustToolchain;
    rustc = rustToolchain;
    CARGO_BUILD_TARGET = "wasm32-unknown-unknown";
    buildPhase = ''
      runHook preBuild
      cargo build \
        -j $NIX_BUILD_CORES \
        --offline \
        --release \
        --target wasm32-unknown-unknown \
        -p sunset-web-wasm \
        --lib
      runHook postBuild
    '';
    installPhase = ''
      runHook preInstall
      wasm-bindgen \
        --target web \
        --out-dir wasm-out \
        target/wasm32-unknown-unknown/release/sunset_web_wasm.wasm
      mkdir -p $out
      cp wasm-out/sunset_web_wasm.js $out/
      cp wasm-out/sunset_web_wasm_bg.wasm $out/
      runHook postInstall
    '';
  };
  ```

- [ ] **Step 2:** Extend the `webDist` derivation's `installPhase` to copy the wasm artifacts into the dist tree. Find the `installPhase` of `webDist` (it currently does `cp -r dist/* $out/`); add right after:

  ```bash
  # Copy WASM bundle alongside the Gleam JS so sunset.ffi.mjs can import it.
  cp ${packages.sunset-web-wasm}/sunset_web_wasm.js $out/
  cp ${packages.sunset-web-wasm}/sunset_web_wasm_bg.wasm $out/
  ```

  But since `webDist` is defined in a `let` binding above `packages = { ... }`, you'll need to either:
  - Bind the wasm derivation in `let` first, then use it in both webDist and packages, OR
  - Move webDist's install-time copy into a `postInstall` hook that references `packages.sunset-web-wasm` via `self.packages.${system}` — but `self` is awkward here.

  Simplest: bind `sunsetWebWasmPkg = pkgs.rustPlatform.buildRustPackage { ... }` at let-level (mirror how Plan D bound `sunsetRelayPkg`), then reference `${sunsetWebWasmPkg}` inside `webDist`'s installPhase, and re-export `sunset-web-wasm = sunsetWebWasmPkg` in `packages`.

- [ ] **Step 3:** Verify:
  ```
  nix build .#sunset-web-wasm --no-link
  ls "$(nix path-info .#sunset-web-wasm 2>/dev/null)"
  nix build .#web --no-link
  ls "$(nix path-info .#web 2>/dev/null)" | grep sunset_web_wasm
  ```

  Both builds succeed; the `web` output contains both the Gleam JS and the wasm artifacts.

- [ ] **Step 4:** Commit:
  ```
  git add flake.nix flake.lock
  git commit -m "Add packages.sunset-web-wasm + integrate into webDist artifact"
  ```

---

### Task 9: Gleam externals (`sunset.gleam` + `sunset.ffi.mjs`)

**Files:**
- Create: `web/src/sunset_web/sunset.gleam`
- Create: `web/src/sunset_web/sunset.ffi.mjs`
- Modify: `web/src/sunset_web/storage.ffi.mjs` (add identity-seed helpers)

- [ ] **Step 1:** Create `web/src/sunset_web/sunset.ffi.mjs`:

  ```javascript
  // Gleam ↔ sunset-web-wasm bridge. Loads the wasm bundle once on first
  // call, caches the module exports, and exposes typed JS functions that
  // Gleam externals call.

  import init, { Client } from "../../sunset_web_wasm.js";

  let initPromise = null;
  function ensureLoaded() {
    if (!initPromise) {
      initPromise = init();
    }
    return initPromise;
  }

  /// Read or generate a 32-byte seed; returns Uint8Array.
  export async function load_or_create_identity() {
    const KEY = "sunset/identity-seed";
    const stored = window.localStorage.getItem(KEY);
    if (stored && /^[0-9a-fA-F]{64}$/.test(stored)) {
      const bytes = new Uint8Array(32);
      for (let i = 0; i < 32; i++) {
        bytes[i] = parseInt(stored.substr(i * 2, 2), 16);
      }
      return bytes;
    }
    const fresh = window.crypto.getRandomValues(new Uint8Array(32));
    const hex = Array.from(fresh, (b) => b.toString(16).padStart(2, "0")).join("");
    window.localStorage.setItem(KEY, hex);
    return fresh;
  }

  /// Construct a Client. Returns the wasm-side Client object as opaque.
  export async function create_client(seed, room_name) {
    await ensureLoaded();
    return new Client(seed, room_name);
  }

  /// Connect to a relay. Returns Result<Nil, String> as Gleam.
  export async function client_add_relay(client, url) {
    try {
      await client.add_relay(url);
      return { Ok: null };
    } catch (e) {
      return { Error: String(e) };
    }
  }

  export async function client_publish_room_subscription(client) {
    try {
      await client.publish_room_subscription();
      return { Ok: null };
    } catch (e) {
      return { Error: String(e) };
    }
  }

  export async function client_send_message(client, body, sent_at_ms) {
    try {
      const nonce = window.crypto.getRandomValues(new Uint8Array(32));
      const value_hash_hex = await client.send_message(body, sent_at_ms, nonce);
      return { Ok: value_hash_hex };
    } catch (e) {
      return { Error: String(e) };
    }
  }

  /// Register the per-message callback. The Gleam side passes a function
  /// that receives an opaque IncomingMessage; we call it.
  export function client_on_message(client, callback) {
    client.on_message((incoming) => {
      callback(incoming);
    });
  }

  export function client_relay_status(client) {
    return client.relay_status;
  }

  /// Read ?relay=<url-encoded> from the current URL. Returns string or null.
  export function relay_url_param() {
    const params = new URLSearchParams(window.location.search);
    return params.get("relay");
  }

  // IncomingMessage accessors — Gleam externals call these to read fields.
  export function incoming_author_pubkey(msg) { return msg.author_pubkey; }
  export function incoming_epoch_id(msg) { return msg.epoch_id; }
  export function incoming_sent_at_ms(msg) { return msg.sent_at_ms; }
  export function incoming_body(msg) { return msg.body; }
  export function incoming_value_hash_hex(msg) { return msg.value_hash_hex; }
  export function incoming_is_self(msg) { return msg.is_self; }
  ```

  Note: the `Result`-shaped return values use `{ Ok: ... }` / `{ Error: ... }` because that's how Gleam's `Result` type is encoded in JavaScript by the Gleam compiler. The Gleam externals can pattern-match on these.

- [ ] **Step 2:** Create `web/src/sunset_web/sunset.gleam`:

  ```gleam
  //// Gleam externals over the sunset-web-wasm bridge.

  pub type ClientHandle
  pub type IncomingMessage

  @external(javascript, "./sunset.ffi.mjs", "load_or_create_identity")
  pub fn load_or_create_identity() -> Promise(BitArray)

  @external(javascript, "./sunset.ffi.mjs", "create_client")
  pub fn create_client(seed: BitArray, room_name: String) -> Promise(ClientHandle)

  @external(javascript, "./sunset.ffi.mjs", "client_add_relay")
  pub fn client_add_relay(client: ClientHandle, url: String) -> Promise(Result(Nil, String))

  @external(javascript, "./sunset.ffi.mjs", "client_publish_room_subscription")
  pub fn client_publish_room_subscription(client: ClientHandle) -> Promise(Result(Nil, String))

  @external(javascript, "./sunset.ffi.mjs", "client_send_message")
  pub fn client_send_message(
    client: ClientHandle,
    body: String,
    sent_at_ms: Int,
  ) -> Promise(Result(String, String))

  @external(javascript, "./sunset.ffi.mjs", "client_on_message")
  pub fn client_on_message(
    client: ClientHandle,
    callback: fn(IncomingMessage) -> Nil,
  ) -> Nil

  @external(javascript, "./sunset.ffi.mjs", "client_relay_status")
  pub fn client_relay_status(client: ClientHandle) -> String

  @external(javascript, "./sunset.ffi.mjs", "relay_url_param")
  pub fn relay_url_param() -> Result(String, Nil)

  // IncomingMessage field accessors.
  @external(javascript, "./sunset.ffi.mjs", "incoming_author_pubkey")
  pub fn incoming_author_pubkey(msg: IncomingMessage) -> BitArray

  @external(javascript, "./sunset.ffi.mjs", "incoming_epoch_id")
  pub fn incoming_epoch_id(msg: IncomingMessage) -> Int

  @external(javascript, "./sunset.ffi.mjs", "incoming_sent_at_ms")
  pub fn incoming_sent_at_ms(msg: IncomingMessage) -> Int

  @external(javascript, "./sunset.ffi.mjs", "incoming_body")
  pub fn incoming_body(msg: IncomingMessage) -> String

  @external(javascript, "./sunset.ffi.mjs", "incoming_value_hash_hex")
  pub fn incoming_value_hash_hex(msg: IncomingMessage) -> String

  @external(javascript, "./sunset.ffi.mjs", "incoming_is_self")
  pub fn incoming_is_self(msg: IncomingMessage) -> Bool

  pub type Promise(t)
  ```

  (The `Promise(t)` type is opaque on the Gleam side; in Lustre apps you'd use `lustre/effect.from_promise` or similar. Confirm against how the existing `web/src/sunset_web/storage.gleam` handles its async externals — likely uses `gleam/javascript/promise` from the JavaScript stdlib.)

- [ ] **Step 3:** **Read existing `web/src/sunset_web/storage.gleam`** to confirm the actual async pattern in use (Gleam Promise vs `gleam/javascript/promise.Promise`). If the project uses `gleam/javascript/promise.Promise`, adjust the imports + types in `sunset.gleam` to match. The plan above uses placeholder `Promise(t)` — replace per the project's actual convention.

- [ ] **Step 4:** Verify the Gleam build:
  ```
  cd web
  nix develop --command gleam build
  ```

  Expect success.

- [ ] **Step 5:** Commit:
  ```
  git add web/src/sunset_web/sunset.gleam web/src/sunset_web/sunset.ffi.mjs
  git commit -m "Add Gleam externals over the sunset-web-wasm bridge"
  ```

---

### Task 10: Wire the bridge into the Gleam app

**Files:**
- Modify: `web/src/sunset_web.gleam`

This task is the biggest Gleam-side change. Replace fixture-driven message data with real engine-driven data.

The plan can't dictate the full diff because `web/src/sunset_web.gleam` is 700+ lines and frontend Claude may evolve it before this task runs. The implementer must:

- [ ] **Step 1:** Read `web/src/sunset_web.gleam` end-to-end to understand the current Model + Msg shape, the `init`, `update`, and `view` functions, and where `fixture.messages()` is currently consumed.

- [ ] **Step 2:** Add a `client: Option(sunset.ClientHandle)` field to the `Model`. Initialize as `None`.

- [ ] **Step 3:** Add new `Msg` variants:
  - `ClientReady(sunset.ClientHandle)` — fired after `create_client` resolves
  - `RelayConnected` / `RelayConnectFailed(String)` — fired after `client_add_relay`
  - `IncomingMsg(sunset.IncomingMessage)` — fired by the on_message callback
  - `ComposeSubmit` — fired by the composer's submit button (replace whatever the existing fixture-driven submit handler dispatches)
  - `ComposeSent(Result(String, String))` — fired after `client_send_message` resolves

- [ ] **Step 4:** Add startup effect chain: in `init`, kick off `load_or_create_identity()` → `create_client(seed, "sunset-demo")` → `ClientReady(handle)`. On `ClientReady`, register `client_on_message` (which dispatches `IncomingMsg`); if `relay_url_param()` returns `Ok(url)`, kick off `client_add_relay` then `client_publish_room_subscription`.

- [ ] **Step 5:** Replace the model's `messages` field source. Currently it reads from `fixture.messages()`; change to `model.messages: List(domain.Message)` accumulated from `IncomingMsg` events. Convert each `sunset.IncomingMessage` to `domain.Message` (the existing UI's render type). Mapping:
  - `id ← sunset.incoming_value_hash_hex(msg)`
  - `author ← short hex of incoming_author_pubkey(msg)` (e.g., first 8 chars; the UI doesn't yet know real display names)
  - `initials ← first 2 chars of author`
  - `time ← format incoming_sent_at_ms(msg)` (HH:MM)
  - `body ← incoming_body(msg)`
  - `you ← incoming_is_self(msg)`
  - `pending ← False`
  - rest default

- [ ] **Step 6:** When the user submits the composer:
  - Optimistic: append a `Message { id: "pending-<random>", body: composer_text, you: True, pending: True, ... }` to `model.messages`. (Or wait for the round-trip — simpler but adds visible latency.)
  - Effect: `client_send_message(handle, body, now_ms)` → `ComposeSent(Result)`.
  - On `ComposeSent(Ok(value_hash_hex))`: optionally update the optimistic placeholder by matching id-prefix; the real `IncomingMsg` will land soon and replace it.
  - On `ComposeSent(Error(_))`: append an error indicator or revert the optimistic message.

  For v0 simplicity: skip the optimistic append. Just send and let the `IncomingMsg` round-trip render it. The latency over localhost is sub-100ms — feels instant.

- [ ] **Step 7:** Add a connection-status indicator in the UI somewhere visible (the existing Member rail has presence dots; add a small "relay: connected/disconnected/error" badge at the top of the channels rail or adjacent to the brand row).

- [ ] **Step 8:** Verify:
  ```
  cd web
  nix develop --command gleam build
  nix develop --command gleam test       # if frontend Claude has Gleam unit tests
  ```

  Both pass.

- [ ] **Step 9:** Commit:
  ```
  git add web/src/sunset_web.gleam
  git commit -m "Wire sunset-web-wasm bridge into the Gleam app"
  ```

  This is the largest single commit in the plan. Keep it self-contained.

---

### Task 11: Playwright e2e test

**Files:**
- Create: `web/e2e/two_browser_chat.spec.js` (or wherever Playwright already looks)

- [ ] **Step 1:** Read `web/playwright.config.js` and any existing tests in `web/e2e/` to understand the project's Playwright conventions. In particular, look for:
  - How they spin up the dev server (Lustre dev / static-web-server / Vite)
  - How they pass URLs between browser contexts
  - Existing globalSetup / fixtures

- [ ] **Step 2:** Write the test. The pattern:

  ```javascript
  import { test, expect, chromium } from "@playwright/test";
  import { spawn } from "child_process";
  import { promisify } from "util";

  let relayProcess = null;
  let relayAddress = null;

  test.beforeAll(async () => {
    // Start sunset-relay on a random port; capture the address from stdout.
    relayProcess = spawn("sunset-relay", []);  // assumes binary on PATH or in nix dev shell
    relayAddress = await new Promise((resolve, reject) => {
      const timeout = setTimeout(() => reject(new Error("relay startup timeout")), 10_000);
      relayProcess.stdout.on("data", (chunk) => {
        const text = chunk.toString();
        const m = text.match(/address: (ws:\/\/[^\s]+)/);
        if (m) {
          clearTimeout(timeout);
          resolve(m[1]);
        }
      });
      relayProcess.stderr.on("data", (chunk) => process.stderr.write(chunk));
    });
  });

  test.afterAll(async () => {
    if (relayProcess) relayProcess.kill();
  });

  test("two browsers exchange a message", async ({ browser }) => {
    const url = `http://localhost:${process.env.PORT || 5173}/?relay=${encodeURIComponent(relayAddress)}`;

    const ctxA = await browser.newContext();
    const ctxB = await browser.newContext();

    const pageA = await ctxA.newPage();
    const pageB = await ctxB.newPage();

    await pageA.goto(url);
    await pageB.goto(url);

    // Wait for relay-status to read "connected" on both.
    await expect(pageA.locator(".relay-status")).toHaveText(/connected/i, { timeout: 10_000 });
    await expect(pageB.locator(".relay-status")).toHaveText(/connected/i, { timeout: 10_000 });

    // A sends; B receives.
    await pageA.fill(".composer input", "hello from A");
    await pageA.press(".composer input", "Enter");

    await expect(pageB.locator(".messages")).toContainText("hello from A", { timeout: 10_000 });

    // Reverse direction.
    await pageB.fill(".composer input", "hello from B");
    await pageB.press(".composer input", "Enter");

    await expect(pageA.locator(".messages")).toContainText("hello from B", { timeout: 10_000 });
  });
  ```

  The selectors (`.relay-status`, `.composer input`, `.messages`) must match the actual class names / data-testid in the Gleam UI. Confirm + adjust to whatever the existing Lustre views render.

- [ ] **Step 3:** Run the test:
  ```
  nix run .#web-test -- two_browser_chat.spec.js
  ```

  (Or however the existing test runner is invoked; check `web/playwright.config.js` for the project's actual command.)

  Expect 1 passed.

- [ ] **Step 4:** Commit:
  ```
  git add web/e2e/two_browser_chat.spec.js
  git commit -m "Add Playwright e2e test: two browsers exchange a message"
  ```

---

### Task 12: Final pass

- [ ] **Step 1:** Workspace-wide checks:
  ```
  nix develop --command cargo fmt --all --check
  nix develop --command cargo clippy --workspace --all-features --all-targets -- -D warnings
  nix develop --command cargo test --workspace --all-features
  ```
  All clean / green.

- [ ] **Step 2:** All wasm builds:
  ```
  nix develop --command cargo build -p sunset-web-wasm --target wasm32-unknown-unknown --lib
  nix develop --command cargo build -p sunset-noise --target wasm32-unknown-unknown --lib
  nix develop --command cargo build -p sunset-core --target wasm32-unknown-unknown --lib
  nix develop --command cargo build -p sunset-sync-ws-browser --target wasm32-unknown-unknown --lib
  ```

- [ ] **Step 3:** All Nix derivations:
  ```
  nix build .#sunset-core-wasm --no-link
  nix build .#sunset-web-wasm --no-link
  nix build .#sunset-relay --no-link
  nix build .#sunset-relay-docker --no-link
  nix build .#web --no-link
  ```

- [ ] **Step 4:** wasm-pack tests:
  ```
  nix develop --command bash -c 'cd crates/sunset-core-wasm && wasm-pack test --node'
  nix develop --command bash -c 'cd crates/sunset-sync-ws-browser && wasm-pack test --node'
  nix develop --command bash -c 'cd crates/sunset-web-wasm && wasm-pack test --node'
  ```

- [ ] **Step 5:** Playwright test:
  ```
  nix run .#web-test -- two_browser_chat.spec.js
  ```

- [ ] **Step 6:** If any cleanup commits were needed:
  ```
  git add -u
  git commit -m "Final fmt + clippy pass"
  ```

---

## Verification (end-state acceptance)

After all 12 tasks land:

- All cargo / clippy / fmt / wasm checks green.
- `nix build .#web` produces a static dist with the Gleam JS + the wasm bundle.
- The Playwright headline test `two browsers exchange a message` passes — proves the entire stack from Gleam UI through wasm bridge through Noise+WebSocket through relay through Noise+WebSocket back to the other Gleam UI works as a coherent system.
- A user running `nix run .#web-dev` and opening two browsers (after starting `nix run .#sunset-relay` in another shell) can chat between them.
- `git log --oneline master..HEAD` — roughly 12 task-by-task commits.

**This is the v0 web demo.**

---

## What this unlocks

After Plan E, the v0 web is real. Natural follow-ups:

- **Plan F — `sunset-store-indexeddb` backend** + persistent client-side history.
- **Plan G — multi-room UI** + room creation / join via shareable URLs.
- **Plan 7 — epoch rotation + key bundles** (per the crypto spec).
- **Plan 8 — membership ops** + invite-only rooms.
- **Plan W — WebRTC transport** (browser ↔ browser direct, browser ↔ relay).
- **PQC subsystem** — unified hybrid post-quantum across Noise + bundles + signatures.
- **Mobile UI work** (frontend Claude's parallel design).
