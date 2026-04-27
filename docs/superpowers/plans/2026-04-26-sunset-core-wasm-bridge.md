# sunset-core-wasm bridge — Implementation Plan

> **For agentic workers:** Use superpowers:executing-plans (or superpowers:subagent-driven-development) to execute this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Land Plan A from the web roadmap. Stand up a new `crates/sunset-core-wasm` crate that exposes seven stateless `sunset-core` functions to JavaScript via `wasm-bindgen`, with a Nix-built `nix build .#sunset-core-wasm` derivation that emits browser-loadable artifacts and a `wasm-pack test --node` integration test that round-trips a real composed→decoded message through the bridge.

**Out of scope (deferred to follow-up plans, per the spec at `docs/superpowers/specs/2026-04-26-sunset-core-wasm-bridge-design.md` § "Items deferred"):**

- WASM exposure of `sunset-store-*` or `sunset-sync` (Plans C/D/E).
- Stream/event subscriptions across the WASM boundary.
- IndexedDB or other persistent browser storage.
- Browser-runner wasm-bindgen tests (we run only under Node).
- Gleam externals or any `web/` wiring (Plan E).

---

## Architecture and design notes

### Crate placement

A new workspace member at `crates/sunset-core-wasm/`. Crate type is `["cdylib", "rlib"]`: `cdylib` is what `wasm-bindgen` needs to emit a usable wasm module; `rlib` keeps `cargo check`/IDE workflows happy.

Bridge surface lives entirely in `src/lib.rs`. No submodules — every `#[wasm_bindgen]` export is one function (or one struct + its constructor functions), and seven exports across one file is well under the threshold where splitting helps.

### Pinned tool versions

`wasm-bindgen-cli`'s ABI must match the `wasm-bindgen` library version exactly, or you get a fatal "Rust ABI mismatch" at runtime. nixpkgs ships `pkgs.wasm-bindgen-cli` at version **0.2.108** today; we pin the workspace dep to `wasm-bindgen = "=0.2.108"` to match. If the nixpkgs version changes in a future flake bump, the workspace pin must be updated in lockstep — both in this plan and in the `crates/sunset-core-wasm/Cargo.toml` line.

`wasm-pack` (used to drive `wasm-bindgen-test` under Node) is a separate tool; nixpkgs provides `pkgs.wasm-pack`. Neither this nor `nodejs` is currently in the dev shell — we add all three (`wasm-bindgen-cli`, `wasm-pack`, `nodejs`) in Task 1.

### RNG approach

Per the spec, the Rust crate does **not** depend on `getrandom`. JS callers supply 32-byte seeds for both `identity_generate` (the Ed25519 keypair seed) and `compose_message` (the AEAD-nonce RNG seed). We feed the second through `rand_chacha::ChaCha20Rng::from_seed` and pass that as the `&mut R` to `fresh_nonce`. This keeps the WASM build pure — no `getrandom = ["js"]` dance.

### Postcard at the boundary

Every "entry" or "block" crossing the JS↔Rust boundary is **postcard-encoded bytes**. JS holds opaque `Uint8Array`s; sunset-sync (in Plan C/D) will pass the same bytes over the wire without re-decoding. Postcard's deterministic encoding makes this safe — what JS holds is the same bytes that came out of `compose_message`, byte-identical.

The bridge re-postcards the `SignedKvEntry` and `ContentBlock` on the way out (compose) and decodes on the way in (decode + verify). Both directions use `sunset_store::SignedKvEntry` / `ContentBlock`'s existing `Serialize`/`Deserialize` derives — no new wire format introduced by this plan.

---

## File structure

```
sunset/
├── Cargo.toml                                  # MODIFY: workspace add sunset-core-wasm member + wasm deps
├── flake.nix                                   # MODIFY: add wasm-bindgen-cli/wasm-pack/nodejs to devShell + new packages.sunset-core-wasm
├── crates/
│   └── sunset-core-wasm/                       # NEW
│       ├── Cargo.toml
│       ├── src/
│       │   └── lib.rs                          # the 7 exports + their structs
│       └── tests/
│           └── web.rs                          # wasm-bindgen-test integration test
```

---

## Tasks

### Task 1: Scaffold the `sunset-core-wasm` crate + extend the dev shell with wasm tooling

**Files:**
- Modify: `Cargo.toml` (workspace root)
- Modify: `flake.nix`
- Create: `crates/sunset-core-wasm/Cargo.toml`
- Create: `crates/sunset-core-wasm/src/lib.rs` (placeholder)

- [ ] **Step 1:** In the root `Cargo.toml`, add to `[workspace.dependencies]`. Insert in alphabetical order:

  ```toml
  rand_chacha = { version = "0.3", default-features = false }
  wasm-bindgen = "=0.2.108"
  wasm-bindgen-test = "0.3"
  ```

  (`=0.2.108` is an exact-version pin so a `cargo update` doesn't drift past `pkgs.wasm-bindgen-cli`'s current nixpkgs version.)

- [ ] **Step 2:** Add `crates/sunset-core-wasm` to the workspace members and a path-dep entry. In the root `Cargo.toml`:

  ```toml
  [workspace]
  members = ["crates/sunset-store", "crates/sunset-store-memory", "crates/sunset-store-fs", "crates/sunset-sync", "crates/sunset-core", "crates/sunset-core-wasm"]
  resolver = "2"
  ```

  (Add to `[workspace.dependencies]` only if other crates will eventually import it — for now, omit; the crate is consumed by the wasm build pipeline, not by other Rust code.)

- [ ] **Step 3:** Create `crates/sunset-core-wasm/Cargo.toml`:

  ```toml
  [package]
  name = "sunset-core-wasm"
  version.workspace = true
  edition.workspace = true
  license.workspace = true
  rust-version.workspace = true

  [lib]
  crate-type = ["cdylib", "rlib"]

  [lints]
  workspace = true

  [dependencies]
  postcard.workspace = true
  rand_chacha.workspace = true
  rand_core.workspace = true
  sunset-core.workspace = true
  sunset-store.workspace = true
  wasm-bindgen.workspace = true

  [dev-dependencies]
  wasm-bindgen-test.workspace = true
  ```

- [ ] **Step 4:** Create `crates/sunset-core-wasm/src/lib.rs` (placeholder, populated in Task 2):

  ```rust
  //! WASM bridge exposing `sunset-core`'s pure functions to JavaScript.
  //!
  //! See `docs/superpowers/specs/2026-04-26-sunset-core-wasm-bridge-design.md`
  //! for the design and the full JS surface contract.

  // Populated in Task 2.
  ```

- [ ] **Step 5:** Modify `flake.nix` to:
  1. Add `pkgs.wasm-bindgen-cli`, `pkgs.wasm-pack`, `pkgs.nodejs` to the dev shell's `buildInputs` (or wherever the existing dev tooling lives — locate the existing `buildInputs` list for the dev shell and append).
  2. (Defer adding the `packages.sunset-core-wasm` derivation to Task 4 to keep the diff for this task focused on the dev-shell change.)

  Verify by running `nix develop --command bash -c 'wasm-bindgen --version && wasm-pack --version && node --version'` from the worktree — all three should print versions; `wasm-bindgen` should print `0.2.108`.

- [ ] **Step 6:** Verify the new crate compiles:

  ```
  nix develop --command cargo build -p sunset-core-wasm
  nix develop --command cargo build -p sunset-core-wasm --target wasm32-unknown-unknown
  ```

  Both should finish cleanly (the placeholder lib has no exports yet, so it's just confirming the dependency tree resolves under both targets).

- [ ] **Step 7:** Commit:

  ```
  git add Cargo.toml flake.nix flake.lock crates/sunset-core-wasm/
  git commit -m "Scaffold sunset-core-wasm crate with wasm-bindgen toolchain"
  ```

  (Include `flake.lock` if the dev-shell change altered it — typically the case when adding new packages.)

---

### Task 2: Implement the seven `#[wasm_bindgen]` exports

**Files:**
- Modify: `crates/sunset-core-wasm/src/lib.rs`

This task replaces the placeholder with the full bridge.

- [ ] **Step 1:** Replace `crates/sunset-core-wasm/src/lib.rs` with:

  ```rust
  //! WASM bridge exposing `sunset-core`'s pure functions to JavaScript.
  //!
  //! See `docs/superpowers/specs/2026-04-26-sunset-core-wasm-bridge-design.md`
  //! for the design and the full JS surface contract.

  use rand_chacha::ChaCha20Rng;
  use rand_core::SeedableRng;
  use wasm_bindgen::prelude::*;

  use sunset_core::{
      ComposedMessage as CoreComposedMessage, Identity, Room,
      compose_message as core_compose, decode_message as core_decode,
      room_messages_filter, Ed25519Verifier,
  };
  use sunset_store::{ContentBlock, SignatureVerifier, SignedKvEntry};

  // ---------------------------------------------------------------------------
  // Helpers
  // ---------------------------------------------------------------------------

  /// Convert a sunset-core/sunset-store error into a `JsError` with a stable
  /// `"sunset-core: <variant>: <display>"` message prefix.
  fn js_err<E: std::fmt::Display>(prefix: &str, e: E) -> JsError {
      JsError::new(&format!("sunset-core: {}: {}", prefix, e))
  }

  fn require_32(label: &str, slice: &[u8]) -> Result<[u8; 32], JsError> {
      <[u8; 32]>::try_from(slice).map_err(|_| {
          JsError::new(&format!(
              "sunset-core: {}: expected 32 bytes, got {}",
              label,
              slice.len(),
          ))
      })
  }

  // ---------------------------------------------------------------------------
  // Identity
  // ---------------------------------------------------------------------------

  #[wasm_bindgen]
  pub struct GeneratedIdentity {
      #[wasm_bindgen(getter_with_clone)]
      pub secret: Vec<u8>,
      #[wasm_bindgen(getter_with_clone)]
      pub public: Vec<u8>,
  }

  /// Derive an Ed25519 identity from a 32-byte caller-supplied seed.
  ///
  /// JS callers should produce the seed via `crypto.getRandomValues(new Uint8Array(32))`.
  #[wasm_bindgen]
  pub fn identity_generate(seed: &[u8]) -> Result<GeneratedIdentity, JsError> {
      let seed = require_32("identity_generate seed", seed)?;
      let id = Identity::from_secret_bytes(&seed);
      Ok(GeneratedIdentity {
          secret: seed.to_vec(),
          public: id.public().as_bytes().to_vec(),
      })
  }

  /// Recover the public half from a stored 32-byte secret seed.
  #[wasm_bindgen]
  pub fn identity_public_from_secret(secret: &[u8]) -> Result<Vec<u8>, JsError> {
      let seed = require_32("identity_public_from_secret secret", secret)?;
      Ok(Identity::from_secret_bytes(&seed)
          .public()
          .as_bytes()
          .to_vec())
  }

  // ---------------------------------------------------------------------------
  // Room
  // ---------------------------------------------------------------------------

  #[wasm_bindgen]
  pub struct OpenedRoom {
      #[wasm_bindgen(getter_with_clone)]
      pub fingerprint: Vec<u8>,
      #[wasm_bindgen(getter_with_clone)]
      pub k_room: Vec<u8>,
      #[wasm_bindgen(getter_with_clone)]
      pub epoch_0_root: Vec<u8>,
  }

  /// Open a room with PRODUCTION Argon2id params.
  ///
  /// Slow (tens to hundreds of ms). JS callers should cache the result per
  /// room name in a session-scoped `Map<string, OpenedRoom>` to avoid paying
  /// the Argon2 cost on every compose / decode.
  #[wasm_bindgen]
  pub fn room_open(name: &str) -> Result<OpenedRoom, JsError> {
      let r = Room::open(name).map_err(|e| js_err("room_open", e))?;
      Ok(OpenedRoom {
          fingerprint: r.fingerprint().as_bytes().to_vec(),
          k_room: r.k_room().to_vec(),
          epoch_0_root: r.epoch_root(0).expect("epoch 0 always present").to_vec(),
      })
  }

  /// Build the `NamePrefix` filter bytes for "all messages in this room".
  ///
  /// Pairs with the entry name format `<hex(fingerprint)>/msg/<hex(value_hash)>`
  /// produced by `compose_message`. JS hands these bytes to sunset-sync (via
  /// later plans) as the subscription filter.
  #[wasm_bindgen]
  pub fn room_messages_filter_prefix(fingerprint: &[u8]) -> Result<Vec<u8>, JsError> {
      // Build a fake Room solely to invoke the helper, OR replicate the
      // formatting locally. We replicate locally to keep this function pure
      // and cheap (no Argon2id).
      let fp = require_32("room_messages_filter_prefix fingerprint", fingerprint)?;
      Ok(format!("{}/msg/", hex::encode(fp)).into_bytes())
  }

  // ---------------------------------------------------------------------------
  // Compose / decode
  // ---------------------------------------------------------------------------

  #[wasm_bindgen]
  pub struct ComposedMessage {
      #[wasm_bindgen(getter_with_clone)]
      pub entry: Vec<u8>,
      #[wasm_bindgen(getter_with_clone)]
      pub block: Vec<u8>,
  }

  /// Compose: full encrypt + sign pipeline.
  ///
  /// Returns postcard-encoded `entry` + `block` bytes. JS hands these to
  /// sunset-sync (Plans C/D/E) for transport + insert.
  #[wasm_bindgen]
  pub fn compose_message(
      secret: &[u8],
      room_name: &str,
      epoch_id: u64,
      sent_at_ms: u64,
      body: &str,
      nonce_seed: &[u8],
  ) -> Result<ComposedMessage, JsError> {
      let secret = require_32("compose_message secret", secret)?;
      let nonce_seed = require_32("compose_message nonce_seed", nonce_seed)?;

      let identity = Identity::from_secret_bytes(&secret);
      let room = Room::open(room_name).map_err(|e| js_err("compose_message room_open", e))?;
      let mut rng = ChaCha20Rng::from_seed(nonce_seed);

      let CoreComposedMessage { entry, block } =
          core_compose(&identity, &room, epoch_id, sent_at_ms, body, &mut rng)
              .map_err(|e| js_err("compose_message", e))?;

      Ok(ComposedMessage {
          entry: postcard::to_stdvec(&entry).map_err(|e| js_err("compose_message entry encode", e))?,
          block: postcard::to_stdvec(&block).map_err(|e| js_err("compose_message block encode", e))?,
      })
  }

  #[wasm_bindgen]
  pub struct DecodedMessage {
      #[wasm_bindgen(getter_with_clone)]
      pub author_pubkey: Vec<u8>,
      pub epoch_id: u64,
      pub sent_at_ms: u64,
      #[wasm_bindgen(getter_with_clone)]
      pub body: String,
  }

  /// Decode: AEAD-decrypt + inner-sig verify.
  ///
  /// Both `entry` and `block` are postcard-encoded. The full sunset-core
  /// authentication invariant is enforced — see the crypto spec § "Authentication
  /// invariant".
  #[wasm_bindgen]
  pub fn decode_message(
      room_name: &str,
      entry: &[u8],
      block: &[u8],
  ) -> Result<DecodedMessage, JsError> {
      let entry: SignedKvEntry =
          postcard::from_bytes(entry).map_err(|e| js_err("decode_message entry decode", e))?;
      let block: ContentBlock =
          postcard::from_bytes(block).map_err(|e| js_err("decode_message block decode", e))?;
      let room = Room::open(room_name).map_err(|e| js_err("decode_message room_open", e))?;

      let decoded = core_decode(&room, &entry, &block)
          .map_err(|e| js_err("decode_message", e))?;

      Ok(DecodedMessage {
          author_pubkey: decoded.author_key.as_bytes().to_vec(),
          epoch_id: decoded.epoch_id,
          sent_at_ms: decoded.sent_at_ms,
          body: decoded.body,
      })
  }

  /// Verify an entry's outer Ed25519 signature.
  ///
  /// JS callers can use this to gate entries received via sunset-sync before
  /// forwarding them into a local store with `Ed25519Verifier` enabled.
  #[wasm_bindgen]
  pub fn verify_entry_signature(entry: &[u8]) -> Result<(), JsError> {
      let entry: SignedKvEntry =
          postcard::from_bytes(entry).map_err(|e| js_err("verify_entry_signature decode", e))?;
      Ed25519Verifier
          .verify(&entry)
          .map_err(|e| js_err("verify_entry_signature", e))?;
      Ok(())
  }

  // Suppress dead-code warnings for the no-longer-used import path triggered
  // by `room_messages_filter` not being called directly above. The import
  // exists so future Plan C/E work that needs the typed `Filter` form can use
  // it without re-importing.
  #[allow(dead_code)]
  fn _ensure_filter_helper_in_scope() {
      let _ = room_messages_filter as fn(&Room) -> sunset_store::Filter;
  }
  ```

- [ ] **Step 2:** Add `hex` to the crate's dependencies (used by `room_messages_filter_prefix`). Edit `crates/sunset-core-wasm/Cargo.toml`'s `[dependencies]` block to add:

  ```toml
  hex.workspace = true
  ```

- [ ] **Step 3:** Format and verify both build targets compile cleanly:

  ```
  nix develop --command cargo fmt -p sunset-core-wasm
  nix develop --command cargo build -p sunset-core-wasm
  nix develop --command cargo build -p sunset-core-wasm --target wasm32-unknown-unknown
  nix develop --command cargo clippy -p sunset-core-wasm --all-targets -- -D warnings
  ```

  All four should finish without warnings or errors.

- [ ] **Step 4:** Commit:

  ```
  git add crates/sunset-core-wasm/Cargo.toml crates/sunset-core-wasm/src/lib.rs
  git commit -m "Add wasm-bindgen exports: identity, room, compose, decode, verify"
  ```

---

### Task 3: `wasm-bindgen-test` integration test under Node

**Files:**
- Create: `crates/sunset-core-wasm/tests/web.rs`

This is the round-trip test that proves every export marshals correctly across the bindgen boundary.

- [ ] **Step 1:** Create `crates/sunset-core-wasm/tests/web.rs`:

  ```rust
  //! End-to-end bridge test: compose a real signed+encrypted message through
  //! the WASM bridge, then decode it back, asserting the author + body
  //! survive the round trip.
  //!
  //! Runs under `wasm-pack test --node`. The native unit tests in
  //! `sunset-core` already cover the underlying logic (134 tests); this
  //! test confirms the wasm-bindgen marshaling for each export round-trips
  //! correctly.

  use sunset_core_wasm::*;
  use wasm_bindgen_test::*;

  wasm_bindgen_test_configure!(run_in_node);

  #[wasm_bindgen_test]
  fn alice_composes_bob_decodes() {
      // Deterministic seeds for reproducibility.
      let alice_seed = [1u8; 32];
      let nonce_seed = [3u8; 32];

      let alice = identity_generate(&alice_seed).expect("identity_generate");

      // Re-derive alice's public key independently and compare.
      let alice_pub_again =
          identity_public_from_secret(&alice.secret).expect("identity_public_from_secret");
      assert_eq!(alice.public, alice_pub_again);

      // Two opens of the same room name yield the same fingerprint + secrets.
      let alice_room = room_open("plan-a-test-room").expect("alice room_open");
      let bob_room = room_open("plan-a-test-room").expect("bob room_open");
      assert_eq!(alice_room.fingerprint, bob_room.fingerprint);
      assert_eq!(alice_room.k_room, bob_room.k_room);
      assert_eq!(alice_room.epoch_0_root, bob_room.epoch_0_root);

      // Filter prefix is `<hex_fingerprint>/msg/`.
      let prefix =
          room_messages_filter_prefix(&alice_room.fingerprint).expect("filter prefix");
      let prefix_str = std::str::from_utf8(&prefix).expect("prefix is utf-8");
      assert!(prefix_str.ends_with("/msg/"));
      assert_eq!(prefix_str.len(), 64 + "/msg/".len());

      // alice composes a real encrypted+signed message.
      let body = "hello bob via wasm bridge";
      let sent_at = 1_700_000_000_000u64;
      let composed = compose_message(
          &alice.secret,
          "plan-a-test-room",
          0,
          sent_at,
          body,
          &nonce_seed,
      )
      .expect("compose_message");

      // outer Ed25519 sig passes (independent of decode).
      verify_entry_signature(&composed.entry).expect("verify_entry_signature");

      // bob decodes (using bob's separately-opened room).
      let decoded = decode_message("plan-a-test-room", &composed.entry, &composed.block)
          .expect("decode_message");

      assert_eq!(decoded.author_pubkey, alice.public);
      assert_eq!(decoded.epoch_id, 0u64);
      assert_eq!(decoded.sent_at_ms, sent_at);
      assert_eq!(decoded.body, body);
  }

  #[wasm_bindgen_test]
  fn identity_generate_rejects_short_seed() {
      let bad_seed = [0u8; 7];
      let err = identity_generate(&bad_seed).expect_err("short seed must fail");
      let msg = format!("{:?}", err);
      assert!(msg.contains("32 bytes"), "error should mention 32-byte requirement: {msg}");
  }

  #[wasm_bindgen_test]
  fn decode_rejects_wrong_room() {
      let alice_seed = [1u8; 32];
      let nonce_seed = [3u8; 32];
      let alice = identity_generate(&alice_seed).expect("identity_generate");

      let composed = compose_message(
          &alice.secret,
          "alice-room",
          0,
          1u64,
          "x",
          &nonce_seed,
      )
      .expect("compose_message");

      // Bob opens a different room and attempts to decode.
      let err = decode_message("eve-room", &composed.entry, &composed.block)
          .expect_err("decode with wrong room must fail");
      let msg = format!("{:?}", err);
      assert!(msg.contains("sunset-core"), "error should be a sunset-core error: {msg}");
  }
  ```

- [ ] **Step 2:** Run the test under wasm-pack + node. From the worktree:

  ```
  nix develop --command bash -c 'cd crates/sunset-core-wasm && wasm-pack test --node'
  ```

  Expected output ends with `test result: ok. 3 passed`. The test runs three `#[wasm_bindgen_test]` cases.

  If wasm-pack reports `wasm-bindgen-cli: 0.2.X mismatch with wasm-bindgen: 0.2.108`, double-check that nixpkgs's `pkgs.wasm-bindgen-cli` version still matches `=0.2.108` in `Cargo.toml`.

- [ ] **Step 3:** Commit:

  ```
  git add crates/sunset-core-wasm/tests/web.rs
  git commit -m "Add wasm-pack node round-trip test for the bridge"
  ```

---

### Task 4: Add `packages.sunset-core-wasm` flake derivation

**Files:**
- Modify: `flake.nix`

The dev-shell change from Task 1 makes `wasm-pack test` runnable interactively. This task adds the production build artifact: `nix build .#sunset-core-wasm` produces `result/sunset_core_wasm.js` + `result/sunset_core_wasm_bg.wasm`.

- [ ] **Step 1:** Add a new derivation to the `packages.<system>` set in `flake.nix`. The shape (adapt to the existing structure of the file — match indentation + the `let` bindings already in scope):

  ```nix
  packages.sunset-core-wasm = pkgs.stdenv.mkDerivation {
    pname   = "sunset-core-wasm";
    version = "0.1.0";
    src     = ./.;

    nativeBuildInputs = [
      rustToolchain                # already defined for the workspace
      pkgs.wasm-bindgen-cli
    ];

    buildPhase = ''
      runHook preBuild
      export CARGO_HOME=$TMPDIR/cargo-home
      cargo build \
        -p sunset-core-wasm \
        --target wasm32-unknown-unknown \
        --release \
        --offline
      wasm-bindgen \
        --target web \
        --out-dir wasm-out \
        target/wasm32-unknown-unknown/release/sunset_core_wasm.wasm
      runHook postBuild
    '';

    installPhase = ''
      runHook preInstall
      mkdir -p $out
      cp wasm-out/sunset_core_wasm.js $out/
      cp wasm-out/sunset_core_wasm_bg.wasm $out/
      runHook postInstall
    '';
  };
  ```

  **Note on `--offline`**: nix builds run sandboxed without network access; cargo dependencies must be vendored or fetched via a fixed-output derivation. Look at how `packages.web` (the sunset-web Gleam derivation) handles its dep fetching — copy the same pattern (likely a `cargoDeps = pkgs.rustPlatform.fetchCargoTarball { ... }` or `crane.buildDepsOnly` invocation). If the existing flake uses `rustPlatform.buildRustPackage`, prefer that wrapper here too (it handles `cargoDeps` automatically and also handles `--target` correctly).

  If `rustPlatform.buildRustPackage` is the existing pattern, the derivation simplifies to roughly:

  ```nix
  packages.sunset-core-wasm = pkgs.rustPlatform.buildRustPackage {
    pname   = "sunset-core-wasm";
    version = "0.1.0";
    src     = ./.;
    cargoLock.lockFile = ./Cargo.lock;
    cargoBuildFlags = [ "-p" "sunset-core-wasm" "--lib" ];
    buildAndTestSubdir = "crates/sunset-core-wasm";
    target = "wasm32-unknown-unknown";
    doCheck = false;  # tests run via wasm-pack in checks.*
    nativeBuildInputs = [ pkgs.wasm-bindgen-cli ];
    postBuild = ''
      wasm-bindgen \
        --target web \
        --out-dir wasm-out \
        target/wasm32-unknown-unknown/release/sunset_core_wasm.wasm
    '';
    installPhase = ''
      mkdir -p $out
      cp wasm-out/sunset_core_wasm.js $out/
      cp wasm-out/sunset_core_wasm_bg.wasm $out/
    '';
  };
  ```

  **Pick whichever shape matches the rest of `flake.nix`.** Read the existing file first, copy its conventions.

- [ ] **Step 2:** Verify the build:

  ```
  nix build .#sunset-core-wasm --no-link --print-out-paths
  ls "$(nix build .#sunset-core-wasm --no-link --print-out-paths)"
  ```

  Expected: two files in the output — `sunset_core_wasm.js` and `sunset_core_wasm_bg.wasm`. Sizes should be roughly:
  - `.js`: 5–15 KB (the wasm-bindgen glue)
  - `.wasm`: ~300–500 KB unstripped, 100–200 KB after `wasm-opt -Oz` (which we don't run yet — defer to a later optimization plan)

- [ ] **Step 3:** Commit:

  ```
  git add flake.nix flake.lock
  git commit -m "Add packages.sunset-core-wasm flake derivation"
  ```

---

### Task 5: Final pass — cross-target builds, lint, fmt, full test, wasm-pack

- [ ] **Step 1:** Workspace-wide lint and format checks (must continue to pass after the new crate):

  ```
  nix develop --command cargo fmt --all --check
  nix develop --command cargo clippy --workspace --all-features --all-targets -- -D warnings
  ```

  Both clean.

- [ ] **Step 2:** Workspace native tests still green:

  ```
  nix develop --command cargo test --workspace --all-features
  ```

  All previous tests pass; sunset-core-wasm itself has no native unit tests (`#[wasm_bindgen_test]` runs under wasm-pack, not native cargo test).

- [ ] **Step 3:** Confirm sunset-core-wasm builds for both targets (native build is for `cargo check` / IDE; wasm build is the artifact path):

  ```
  nix develop --command cargo build -p sunset-core-wasm
  nix develop --command cargo build -p sunset-core-wasm --target wasm32-unknown-unknown --release
  ```

- [ ] **Step 4:** Run the wasm-pack node test one more time end-to-end:

  ```
  nix develop --command bash -c 'cd crates/sunset-core-wasm && wasm-pack test --node'
  ```

  Expect 3 passed.

- [ ] **Step 5:** Confirm the flake derivation builds:

  ```
  nix build .#sunset-core-wasm --no-link
  ```

  Expect `Finished`.

- [ ] **Step 6:** If any cleanup commits were needed in Steps 1–2, commit:

  ```
  git add -u
  git commit -m "Final fmt + clippy pass"
  ```

---

## Verification (end-state acceptance)

After all 5 tasks land:

- `nix develop --command cargo test --workspace --all-features` — green, including all tests from prior plans (no regressions).
- `nix develop --command cargo clippy --workspace --all-features --all-targets -- -D warnings` — clean.
- `nix develop --command cargo fmt --all --check` — clean.
- `nix develop --command bash -c 'cd crates/sunset-core-wasm && wasm-pack test --node'` — 3 passed.
- `nix build .#sunset-core-wasm --no-link` — produces `result/sunset_core_wasm.js` + `result/sunset_core_wasm_bg.wasm`.
- The 7 wasm-bindgen exports listed in the spec are all present and verified by the round-trip integration test.
- `git log --oneline master..HEAD` — roughly 5 task-by-task commits (more if any clippy fixes land mid-task).

---

## What this unlocks

After Plan A:

- **Plan C — WebSocket transport for `sunset-sync`** (with sync-internal Ed25519 signing folded in). Adds the Rust-side `WebSocketTransport` impl + the SyncEngine signing path.
- **Plan D — `sunset-relay` binary + Docker image.** Native sunset-core + sunset-store-fs + sunset-sync (with the new transport) + a WS listener.
- **Plan E — Gleam UI wires to the WASM bridge.** Replaces fixture data in `web/` with live calls to the bridge's 7 functions, plus calls into the not-yet-existing browser-side sync engine wrapper. Plan E may need a small additional bridge (Plan A.1?) to expose the not-yet-bridged sunset-store-memory + sunset-sync engine to JS — that's a follow-up sizing decision once Plans C and D land.
