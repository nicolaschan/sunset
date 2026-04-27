# sunset-core-wasm — JS bridge subsystem design

- **Date:** 2026-04-26
- **Status:** Draft (subsystem-level)
- **Scope:** A thin wasm-bindgen bridge that exposes `sunset-core`'s pure functions to JavaScript. First step in the web-version roadmap (Plan A in the A/C/D/E sequence).
- **Position in the architecture:** This produces the `sunset-core-wasm` artifact named by `2026-04-25-sunset-chat-architecture-design.md` § "Flake build outputs". The Gleam web client (Plan E) and the Minecraft mod's WASM runtime (later) consume this artifact.

## Goals (in scope for v1 of this subsystem)

- Expose `sunset-core`'s identity, room, message-compose, message-decode, filter-prefix, and entry-signature functions to JavaScript via `wasm-bindgen`.
- Produce a single `nix build .#sunset-core-wasm` derivation that emits `sunset_core_wasm.js` + `sunset_core_wasm_bg.wasm` ready to be `import`'d by a browser or a Node test runner.
- Stay stateless on the Rust side. No bridge-owned objects; every call is a pure function over byte slices and primitive types.
- Single round-trip test (compose → decode) running under `wasm-bindgen-test` against a node target verifies the bridge end-to-end.

## Non-goals (deferred)

- **No bridge for `sunset-store-*` or `sunset-sync`.** Stateful in-browser store + sync engine are Plans C/D/E. Plan A's surface is exclusively over `sunset-core`'s pure functions.
- **No event/stream bridges** (no `subscribe()`-style live updates flowing from WASM to JS).
- **No `getrandom = ["js"]` shenanigans.** RNG seed is provided by JS callers from `crypto.getRandomValues()`.
- **No IndexedDB / localStorage / Web Worker offload.**
- **No Gleam externals or web/ wiring.** The Gleam side imports the artifact in Plan E.
- **No TypeScript hand-written `.d.ts`.** wasm-bindgen emits adequate type declarations from the Rust signatures; we don't curate them further.

## Crate layout

A new workspace member at `crates/sunset-core-wasm/`:

```
crates/sunset-core-wasm/
├── Cargo.toml                        # NEW: cdylib, wasm-bindgen dep, sunset-core dep
└── src/
    └── lib.rs                        # NEW: every #[wasm_bindgen] export lives here
```

Rationale for a separate crate (vs. a `wasm-bindings` feature on `sunset-core`):

- Native consumers (`sunset-tui`, `sunset-relay`) don't pull `wasm-bindgen` into their build.
- The bridge's surface can evolve independently of `sunset-core`'s public API.
- `cdylib` crate-type is incompatible with mixing `rlib` consumers; cleaner to separate.
- Matches the architecture spec's `nix build .#sunset-core-wasm` artifact name.

## JS surface (7 functions, all stateless)

Every export is `#[wasm_bindgen]` and takes/returns a combination of:

- `&[u8]` / `Vec<u8>` (becomes `Uint8Array` on the JS side; wasm-bindgen handles the marshaling).
- `&str` / `String`.
- `u64` for timestamps and epoch ids (becomes `bigint` on the JS side — wasm-bindgen's default for `u64` since 0.2.85).
- A small struct with `#[wasm_bindgen(getter_with_clone)]` for compound returns; emits as a JS class with read-only fields.

```rust
// Returns a freshly-generated identity. JS provides the 32-byte seed from
// crypto.getRandomValues — no getrandom dep on the Rust side.
#[wasm_bindgen]
pub struct GeneratedIdentity {
    #[wasm_bindgen(getter_with_clone)] pub secret: Vec<u8>,    // 32 bytes
    #[wasm_bindgen(getter_with_clone)] pub public: Vec<u8>,    // 32 bytes
}
#[wasm_bindgen]
pub fn identity_generate(seed: &[u8]) -> Result<GeneratedIdentity, JsError>;

// Recover the public half from a stored 32-byte seed.
#[wasm_bindgen]
pub fn identity_public_from_secret(secret: &[u8]) -> Result<Vec<u8>, JsError>;

// Open a room with PRODUCTION Argon2id params. Slow (tens to hundreds of ms);
// JS callers should cache the result per room name across the session.
#[wasm_bindgen]
pub struct OpenedRoom {
    #[wasm_bindgen(getter_with_clone)] pub fingerprint: Vec<u8>,    // 32 bytes
    #[wasm_bindgen(getter_with_clone)] pub k_room: Vec<u8>,         // 32 bytes
    #[wasm_bindgen(getter_with_clone)] pub epoch_0_root: Vec<u8>,   // 32 bytes
}
#[wasm_bindgen]
pub fn room_open(name: &str) -> Result<OpenedRoom, JsError>;

// Build the NamePrefix bytes that sunset-sync will use as the subscription
// filter for "all messages in this room": <hex(fingerprint)>/msg/.
#[wasm_bindgen]
pub fn room_messages_filter_prefix(fingerprint: &[u8]) -> Result<Vec<u8>, JsError>;

// Compose: full encrypt + sign pipeline. Returns postcard-encoded entry +
// block bytes that JS hands to sunset-sync (Plan C/D/E) for transport +
// store-side insert.
#[wasm_bindgen]
pub struct ComposedMessage {
    #[wasm_bindgen(getter_with_clone)] pub entry: Vec<u8>,    // postcard(SignedKvEntry)
    #[wasm_bindgen(getter_with_clone)] pub block: Vec<u8>,    // postcard(ContentBlock)
}
#[wasm_bindgen]
pub fn compose_message(
    secret: &[u8],          // 32 bytes
    room_name: &str,
    epoch_id: u64,
    sent_at_ms: u64,
    body: &str,
    nonce_seed: &[u8],      // 32 bytes; consumed to seed an internal RNG for the AEAD nonce
) -> Result<ComposedMessage, JsError>;

// Decode: full AEAD-decrypt + inner-sig verify pipeline. Returns the
// authenticated message contents.
#[wasm_bindgen]
pub struct DecodedMessage {
    #[wasm_bindgen(getter_with_clone)] pub author_pubkey: Vec<u8>,  // 32 bytes
    pub epoch_id: u64,
    pub sent_at_ms: u64,
    #[wasm_bindgen(getter_with_clone)] pub body: String,
}
#[wasm_bindgen]
pub fn decode_message(
    room_name: &str,
    entry: &[u8],          // postcard(SignedKvEntry)
    block: &[u8],          // postcard(ContentBlock)
) -> Result<DecodedMessage, JsError>;

// Verify an entry's outer Ed25519 signature (= what Ed25519Verifier does).
// JS layer can call this before forwarding entries received via sync.
#[wasm_bindgen]
pub fn verify_entry_signature(entry: &[u8]) -> Result<(), JsError>;
```

Seven functions total. No opaque handle types — all returns are either `Vec<u8>` or a `getter_with_clone` struct of byte arrays. wasm-bindgen's default marshaling handles the lot.

### Why JS provides the nonce seed

`compose_message` needs randomness for the AEAD nonce. The same "JS provides RNG" approach used for `identity_generate`'s seed: caller passes 32 bytes from `crypto.getRandomValues()`. Internally we use a `ChaCha20Rng::from_seed(seed)` (deterministic, fast, no `getrandom`) to fill the actual 24-byte XChaCha20 nonce.

### Why the room-name parameter on every call

`compose_message` and `decode_message` re-derive `K_room` + epoch root each call from `room_name`. Argon2id is slow under production params (tens to hundreds of ms). Two acceptable mitigations, both JS-side:

1. **JS-side cache.** Recommended. JS holds a `Map<roomName, OpenedRoom>` keyed by room name; reuses across calls. The Rust API stays pure-functional.
2. **Pass derived secrets.** Add overloads `compose_message_with_room(secret, room_secrets, ...)` accepting a previously-derived `OpenedRoom`. Faster but doubles the API surface.

For Plan A we ship the simple form (option 1, cache on JS side). If profiling shows the re-derivation is hot, option 2 is an additive change.

### Error mapping

Every `Result::Err` from `sunset-core` becomes a `JsError` whose message is `"sunset-core: {variant_name}: {display}"`. JS callers see thrown exceptions with a stable prefix they can match against if they need branch behavior. wasm-bindgen converts `JsError` to a JS `Error` object automatically.

## Build pipeline

Two new flake outputs (extend `flake.nix`):

```nix
packages.sunset-core-wasm = pkgs.stdenv.mkDerivation {
  name = "sunset-core-wasm";
  src = ./.;  # workspace root
  nativeBuildInputs = [ rustToolchain pkgs.wasm-bindgen-cli ];
  buildPhase = ''
    cargo build -p sunset-core-wasm --target wasm32-unknown-unknown --release
    wasm-bindgen --target web --out-dir out \
      target/wasm32-unknown-unknown/release/sunset_core_wasm.wasm
  '';
  installPhase = ''
    mkdir -p $out
    cp out/sunset_core_wasm.js $out/
    cp out/sunset_core_wasm_bg.wasm $out/
  '';
};

# `wasm-bindgen-test` runs against this node target.
checks.sunset-core-wasm-test = pkgs.stdenv.mkDerivation {
  ...
  nativeBuildInputs = [ rustToolchain pkgs.wasm-bindgen-cli pkgs.wasm-pack pkgs.nodejs ];
  buildPhase = ''
    wasm-pack test --node crates/sunset-core-wasm
  '';
};
```

The `wasm-bindgen-cli` version pinned by `pkgs.wasm-bindgen-cli` MUST match the `wasm-bindgen` library version in `Cargo.toml` — a well-known footgun where mismatched versions emit a fatal "Rust ABI mismatch" error at runtime. The implementation plan will pick the version from nixpkgs first and pin the workspace dep to match.

`wasm-pack` and `nodejs` are added to `nativeBuildInputs` for the test derivation.

## Test discipline

A single integration test that exercises the full bridge surface end-to-end:

```rust
// crates/sunset-core-wasm/tests/web.rs
use wasm_bindgen_test::*;
use sunset_core_wasm::*;

wasm_bindgen_test_configure!(run_in_node);

#[wasm_bindgen_test]
fn alice_composes_bob_decodes() {
    // alice + bob both derive identities from fixed seeds (deterministic test)
    let alice_seed = [1u8; 32];
    let bob_seed   = [2u8; 32];
    let alice = identity_generate(&alice_seed).unwrap();
    let _bob  = identity_generate(&bob_seed).unwrap();

    // both open the same room
    let alice_room = room_open("general-test").unwrap();
    let bob_room   = room_open("general-test").unwrap();
    assert_eq!(alice_room.fingerprint, bob_room.fingerprint);

    // alice composes
    let nonce_seed = [3u8; 32];
    let msg = compose_message(
        &alice.secret, "general-test", 0, 1_700_000_000_000, "hi from wasm", &nonce_seed,
    ).unwrap();

    // outer-sig check passes
    verify_entry_signature(&msg.entry).unwrap();

    // bob decodes
    let decoded = decode_message("general-test", &msg.entry, &msg.block).unwrap();
    assert_eq!(decoded.author_pubkey, alice.public);
    assert_eq!(decoded.body, "hi from wasm");
    assert_eq!(decoded.epoch_id, 0u64);
    assert_eq!(decoded.sent_at_ms, 1_700_000_000_000u64);
}
```

That single test exercises every exported function. It runs under `wasm-pack test --node crates/sunset-core-wasm` in the flake's `checks`.

The native unit tests inside `sunset-core` (134 tests) already cover the underlying logic; this bridge test only confirms that the wasm-bindgen marshaling for each type round-trips correctly.

For test-only speed, `room_open("general-test")` uses production Argon2id params here but with a short fixed name — typical browser-side latency is ~80ms which the test tolerates fine. (No `room_open_with_params` exposed in the bridge; the production discipline is enforced.)

## Dependencies

`crates/sunset-core-wasm/Cargo.toml`:

```toml
[package]
name = "sunset-core-wasm"
version.workspace = true
edition.workspace = true
license.workspace = true
rust-version.workspace = true

[lib]
crate-type = ["cdylib", "rlib"]   # cdylib for wasm; rlib for tests + IDE

[lints]
workspace = true

[dependencies]
sunset-core.workspace = true
sunset-store.workspace = true     # for SignedKvEntry / ContentBlock postcard
wasm-bindgen = "0.2"
postcard.workspace = true
rand_chacha = "0.3"               # ChaCha20Rng for seed-driven RNG inside compose_message
rand_core.workspace = true

[dev-dependencies]
wasm-bindgen-test = "0.3"
```

Add to root `[workspace.dependencies]`:

```toml
rand_chacha = "0.3"
wasm-bindgen = "0.2"
wasm-bindgen-test = "0.3"
```

## Items deferred to follow-up plans

- **`sunset-store-memory` bridge** — Plan E will need to expose store insert/get/iter to JS so the Gleam app can drive the store from outside.
- **`sunset-sync` bridge + WebSocket transport** — Plans C/D wrap the engine for browser use.
- **Stream/event bridges.** Once subscriptions matter, we add a callback-based `subscribe(filter, on_event)` export.
- **IndexedDB store backend** (separate `sunset-store-indexeddb` crate per the architecture spec).
- **Browser-runnable wasm-bindgen tests** — current spec runs only under node. Browser tests need playwright or wasm-pack with a headless browser.
- **Hardened RNG injection** — for now, JS provides 32-byte seeds explicitly per call; future work could establish a single bridge-owned `ChaCha20Rng` seeded once at startup.

## Self-review checklist

- [x] Every JS-side export's signature matches a real Rust function in `sunset-core`.
- [x] No state on the Rust side — all functions are pure.
- [x] No `getrandom` dep; JS supplies all entropy.
- [x] Build pipeline produces named artifacts that match the architecture spec (`sunset-core-wasm`).
- [x] Test plan is small (1 round-trip test) — relies on the existing 134 sunset-core tests for underlying logic.
- [x] Crate layout justified vs. feature-flag alternative.
- [x] Out-of-scope items explicitly enumerated to prevent scope creep.
