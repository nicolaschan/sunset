# Tracing Migration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace ad-hoc logging (`eprintln!`, `web_sys::console::*`) across the workspace with `tracing` macros, wire `wasm-tracing` as the WASM subscriber, and remove the duplicate startup-banner emit in `sunset-relay`.

**Architecture:** Library crates depend only on the `tracing` facade (no subscriber). The native binary (`sunset-relay`) keeps its existing `tracing-subscriber`/`EnvFilter` setup. The browser bundle (`sunset-web-wasm`) installs a `wasm-tracing::WasmLayer` once via `#[wasm_bindgen(start)]`, which forwards events to `console.log/warn/error`. The conversion follows the level-mapping table in the spec — most `eprintln!` calls become `tracing::warn!`, lifecycle events become `info!`, adversary-controllable lines become `debug!`.

**Tech Stack:** Rust workspace (`tracing 0.1`, `tracing-subscriber 0.3` already present; adding `wasm-tracing 2.1`), `wasm-bindgen`, Nix-pinned toolchain (`nix develop --command cargo …`).

**Spec:** `docs/superpowers/specs/2026-05-02-tracing-migration-design.md`.

**Working tree:** All work happens in the existing worktree `.worktrees/tracing-migration` on branch `tracing-migration`. Every cargo command runs through `nix develop --command …` per the project's hermeticity rule.

**No new tests.** This is a side-effect refactor; the existing test suite must continue to pass. Per-task verification is a build/clippy run, not new test code.

---

## Task 1: Add `wasm-tracing` to workspace dependencies

**Files:**
- Modify: `Cargo.toml` (workspace-root)

- [ ] **Step 1: Add the workspace dep**

In `Cargo.toml`, find the `[workspace.dependencies]` block. Locate the existing line:

```toml
tracing-subscriber = { version = "0.3", default-features = false, features = ["env-filter", "fmt"] }
```

Add directly underneath:

```toml
wasm-tracing = "2.1"
```

(Pin to the 2.1 line; `wasm-tracing` 2.1.0 is the version whose API the spec uses: `set_as_global_default_with_config(WasmLayerConfig)`. It transitively pulls `tracing-subscriber` for its registry — that's expected and intentional.)

- [ ] **Step 2: Resolve the lockfile**

Run: `nix develop --command cargo metadata --quiet >/dev/null`

Expected: completes silently. `Cargo.lock` will show a new `wasm-tracing 2.1.x` entry plus its transitive deps. No code uses the dep yet, so nothing else changes.

- [ ] **Step 3: Confirm workspace still builds**

Run: `nix develop --command cargo check --workspace --all-features`
Expected: `Finished` with no warnings or errors.

- [ ] **Step 4: Commit**

```bash
git add Cargo.toml Cargo.lock
git commit -m "$(cat <<'EOF'
deps: add wasm-tracing 2.1 workspace dependency

Pinned for use in sunset-web-wasm to forward tracing events to the
browser console. No call sites yet; this commit only adds the dep.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 2: Convert `sunset-sync` engine.rs to `tracing`

**Files:**
- Modify: `crates/sunset-sync/Cargo.toml`
- Modify: `crates/sunset-sync/src/engine.rs` (lines 403, 411, 596, 629, 637, 723, 799, 974)

- [ ] **Step 1: Add `tracing` to the crate's dependencies**

In `crates/sunset-sync/Cargo.toml`, find the `[dependencies]` block (it currently starts with `async-trait.workspace = true`). Add this line in alphabetical position (between `thiserror` and `tokio`):

```toml
tracing.workspace = true
```

The full neighbourhood should read:

```toml
sunset-store.workspace = true
thiserror.workspace = true
tokio = { workspace = true, features = ["sync", "rt", "macros", "time"] }
tracing.workspace = true
web-time.workspace = true
```

- [ ] **Step 2: Replace `engine.rs:403` (transport accept failed)**

In `crates/sunset-sync/src/engine.rs` line 403, change:

```rust
                            eprintln!("sunset-sync: transport accept failed; continuing: {e}");
```

to:

```rust
                            tracing::warn!(error = %e, "transport accept failed; continuing");
```

(Drop the `sunset-sync:` prefix; `tracing` records `target = module_path!()` automatically. Move `e` into a structured `error` field with `%` for `Display`.)

- [ ] **Step 3: Replace `engine.rs:411-414` (accept timed out)**

Change:

```rust
                            eprintln!(
                                "sunset-sync: transport accept timed out after {:?}; continuing",
                                self.config.accept_handshake_timeout
                            );
```

to:

```rust
                            tracing::warn!(
                                timeout = ?self.config.accept_handshake_timeout,
                                "transport accept timed out; continuing",
                            );
```

(The `?` sigil is `Debug` formatting, matching the original `{:?}`.)

- [ ] **Step 4: Replace `engine.rs:596` (peer disconnected)**

Change:

```rust
                eprintln!("sunset-sync: peer {peer_id:?} disconnected ({conn_id}): {reason}");
```

to:

```rust
                tracing::info!(
                    peer_id = ?peer_id,
                    conn_id = %conn_id,
                    reason = %reason,
                    "peer disconnected",
                );
```

(`info` per the spec table — this is a lifecycle event operators want to see by default.)

- [ ] **Step 5: Replace `engine.rs:629` (replay subscriptions iter error)**

Change:

```rust
                    eprintln!("sunset-sync: replay_existing_subscriptions: {e}");
```

to:

```rust
                    tracing::warn!(error = %e, "replay_existing_subscriptions: store iteration");
```

- [ ] **Step 6: Replace `engine.rs:637` (replay subscriptions get_content error)**

Change:

```rust
                    eprintln!("sunset-sync: replay_existing_subscriptions: {e}");
```

to:

```rust
                    tracing::warn!(error = %e, "replay_existing_subscriptions: get_content");
```

(Differentiate from line 629 — same severity, but the call site is different. Disambiguating message helps log readers.)

- [ ] **Step 7: Replace `engine.rs:723` (digest scan failed)**

Change:

```rust
                eprintln!("sunset-sync: digest scan failed: {e}");
```

to:

```rust
                tracing::warn!(error = %e, "digest scan failed");
```

- [ ] **Step 8: Replace `engine.rs:799-802` (insert failed for delivered entry)**

Change:

```rust
                    eprintln!(
                        "sunset-sync: insert failed for entry from {:?}: {}",
                        entry.verifying_key, e
                    );
```

to:

```rust
                    tracing::warn!(
                        verifying_key = ?entry.verifying_key,
                        error = %e,
                        "insert failed for delivered entry",
                    );
```

(This is a swallowed error followed by `continue;` — `warn` per the spec table.)

- [ ] **Step 9: Replace `engine.rs:974` (bad-signature ephemeral datagram)**

Change:

```rust
            eprintln!("sunset-sync: dropping ephemeral datagram from {from:?} — bad signature");
```

to:

```rust
            tracing::debug!(from = ?from, "dropping ephemeral datagram — bad signature");
```

(`debug` per the spec table — adversary-controllable line rate.)

- [ ] **Step 10: Build and clippy the crate**

Run: `nix develop --command cargo build -p sunset-sync --all-features`
Expected: `Finished` with no warnings.

Run: `nix develop --command cargo clippy -p sunset-sync --all-features --all-targets -- -D warnings`
Expected: `Finished` with no warnings.

- [ ] **Step 11: Run the crate's tests**

Run: `nix develop --command cargo test -p sunset-sync --all-features`
Expected: all tests pass.

- [ ] **Step 12: Confirm no `eprintln!` remains in the crate**

Run: `nix develop --command bash -c "rg -n 'eprintln!' crates/sunset-sync/ || echo OK"`
Expected: prints `OK`.

- [ ] **Step 13: Commit**

```bash
git add crates/sunset-sync/Cargo.toml crates/sunset-sync/src/engine.rs
git commit -m "$(cat <<'EOF'
sunset-sync: route engine logs through tracing

Replaces the eight eprintln! sites in engine.rs with tracing::{warn,info,
debug}! per the migration design's level-mapping table. Adds a tracing
workspace dep on sunset-sync; no subscriber — that's the binary's job.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 3: Convert `sunset-core` membership.rs to `tracing`

**Files:**
- Modify: `crates/sunset-core/Cargo.toml`
- Modify: `crates/sunset-core/src/membership.rs` (lines 287, 305)

- [ ] **Step 1: Add `tracing` to the crate's dependencies**

In `crates/sunset-core/Cargo.toml`, find the `[dependencies]` block. Insert in alphabetical position (between `thiserror` and `tokio`):

```toml
tracing.workspace = true
```

The neighbourhood should read:

```toml
sunset-sync.workspace = true
thiserror.workspace = true
tokio = { workspace = true, features = ["sync", "time"] }
tokio-stream = { workspace = true }
tracing.workspace = true
web-time.workspace = true
```

- [ ] **Step 2: Replace `membership.rs:287` (presence subscribe failed)**

Change:

```rust
                eprintln!("MembershipTracker: presence subscribe failed: {e}");
```

to:

```rust
                tracing::warn!(error = %e, "presence subscribe failed");
```

- [ ] **Step 3: Replace `membership.rs:305` (presence event error)**

Change:

```rust
                            eprintln!("MembershipTracker presence event: {e}");
```

to:

```rust
                            tracing::warn!(error = %e, "presence event error");
```

- [ ] **Step 4: Build and clippy the crate**

Run: `nix develop --command cargo build -p sunset-core --all-features`
Expected: `Finished` with no warnings.

Run: `nix develop --command cargo clippy -p sunset-core --all-features --all-targets -- -D warnings`
Expected: `Finished` with no warnings.

- [ ] **Step 5: Run the crate's tests**

Run: `nix develop --command cargo test -p sunset-core --all-features`
Expected: all tests pass.

- [ ] **Step 6: Confirm no `eprintln!` remains in the crate**

Run: `nix develop --command bash -c "rg -n 'eprintln!' crates/sunset-core/ || echo OK"`
Expected: prints `OK`.

- [ ] **Step 7: Commit**

```bash
git add crates/sunset-core/Cargo.toml crates/sunset-core/src/membership.rs
git commit -m "$(cat <<'EOF'
sunset-core: route membership logs through tracing

Replaces both eprintln! sites in membership.rs with tracing::warn!.
Adds a tracing workspace dep on sunset-core.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 4: Convert `sunset-sync-webrtc-browser` wasm.rs to `tracing`

**Files:**
- Modify: `crates/sunset-sync-webrtc-browser/Cargo.toml`
- Modify: `crates/sunset-sync-webrtc-browser/src/wasm.rs` (line 421)

- [ ] **Step 1: Add `tracing` to the crate's dependencies**

In `crates/sunset-sync-webrtc-browser/Cargo.toml`, add to the `[dependencies]` block (alphabetical position):

```toml
tracing.workspace = true
```

If you're unsure where it goes, put it just before `web-sys.workspace = true`.

- [ ] **Step 2: Replace `wasm.rs:421-427` (unknown datachannel label)**

Change:

```rust
                web_sys::console::warn_1(
                    &format!(
                        "sunset-sync: ignoring unknown datachannel label '{}'",
                        other
                    )
                    .into(),
                );
```

to:

```rust
                tracing::warn!(label = %other, "ignoring unknown datachannel label");
```

(Drop the `sunset-sync:` prefix — `target = module_path!()` covers attribution.)

- [ ] **Step 3: Build the crate for native and WASM**

Run: `nix develop --command cargo build -p sunset-sync-webrtc-browser`
Expected: `Finished` with no warnings.

Run: `nix develop --command cargo build -p sunset-sync-webrtc-browser --target wasm32-unknown-unknown`
Expected: `Finished` with no warnings.

- [ ] **Step 4: Clippy**

Run: `nix develop --command cargo clippy -p sunset-sync-webrtc-browser --all-features --all-targets -- -D warnings`
Expected: `Finished` with no warnings.

- [ ] **Step 5: Confirm no `web_sys::console` remains**

Run: `nix develop --command bash -c "rg -n 'web_sys::console' crates/sunset-sync-webrtc-browser/ || echo OK"`
Expected: prints `OK`.

- [ ] **Step 6: Commit**

```bash
git add crates/sunset-sync-webrtc-browser/Cargo.toml crates/sunset-sync-webrtc-browser/src/wasm.rs
git commit -m "$(cat <<'EOF'
sunset-sync-webrtc-browser: route the unknown-label warning through tracing

The single web_sys::console::warn_1 call in wasm.rs becomes
tracing::warn!. Adds tracing workspace dep.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 5: Convert `sunset-web-wasm` console calls to `tracing`

This task converts every `web_sys::console::{error,warn}_1` site in the four affected files and updates the stale doc comment.

**Files:**
- Modify: `crates/sunset-web-wasm/Cargo.toml`
- Modify: `crates/sunset-web-wasm/src/client.rs` (lines 98, 394, 407, 416, 424, 477, 492, 499)
- Modify: `crates/sunset-web-wasm/src/presence_publisher.rs` (line 30)
- Modify: `crates/sunset-web-wasm/src/relay_signaler.rs` (lines 165, 177, 184)
- Modify: `crates/sunset-web-wasm/src/voice.rs` (line 72)

- [ ] **Step 1: Add `tracing` to the crate's dependencies**

In `crates/sunset-web-wasm/Cargo.toml`, add to the main `[dependencies]` block (alphabetical position, before the wasm-only target block):

```toml
tracing.workspace = true
```

For example, place it between `thiserror.workspace = true` and `tokio = { workspace = true, features = ["sync"] }`.

- [ ] **Step 2: Replace `client.rs:98` (sync engine exited)**

Change:

```rust
                web_sys::console::error_1(&JsValue::from_str(&format!("sync engine exited: {e}")));
```

to:

```rust
                tracing::error!(error = %e, "sync engine exited");
```

- [ ] **Step 3: Replace `client.rs:394` (store.subscribe failed)**

Change:

```rust
                    web_sys::console::error_1(&JsValue::from_str(&format!("store.subscribe: {e}")));
```

to:

```rust
                    tracing::error!(error = %e, "store.subscribe failed");
```

- [ ] **Step 4: Replace `client.rs:407` (store event error)**

Change:

```rust
                        web_sys::console::error_1(&JsValue::from_str(&format!("store event: {e}")));
```

to:

```rust
                        tracing::error!(error = %e, "store event");
```

- [ ] **Step 5: Replace `client.rs:416` (get_content failed)**

Change:

```rust
                        web_sys::console::error_1(&JsValue::from_str(&format!("get_content: {e}")));
```

to:

```rust
                        tracing::error!(error = %e, "get_content");
```

- [ ] **Step 6: Replace `client.rs:424-426` (decode_message)**

Change:

```rust
                        web_sys::console::error_1(&JsValue::from_str(&format!(
                            "decode_message: {e}"
                        )));
```

to:

```rust
                        tracing::error!(error = %e, "decode_message");
```

- [ ] **Step 7: Update the stale doc comment at `client.rs:477`**

Change the line:

```rust
/// Errors are logged via `web_sys::console` and swallowed — receipts
```

to:

```rust
/// Errors are logged via `tracing` and swallowed — receipts
```

- [ ] **Step 8: Replace `client.rs:492-494` (compose_receipt failed)**

Change:

```rust
                web_sys::console::error_1(&wasm_bindgen::JsValue::from_str(&format!(
                    "compose_receipt failed: {e}"
                )));
```

to:

```rust
                tracing::error!(error = %e, "compose_receipt failed");
```

- [ ] **Step 9: Replace `client.rs:499-501` (store.insert(receipt) failed)**

Change:

```rust
        web_sys::console::error_1(&wasm_bindgen::JsValue::from_str(&format!(
            "store.insert(receipt) failed: {e}"
        )));
```

to:

```rust
        tracing::error!(error = %e, "store.insert(receipt) failed");
```

- [ ] **Step 10: Replace `presence_publisher.rs:30`**

Change:

```rust
                web_sys::console::warn_1(&JsValue::from_str(&format!("presence publisher: {e}")));
```

to:

```rust
                tracing::warn!(error = %e, "presence publisher");
```

- [ ] **Step 11: Replace `relay_signaler.rs:165-167` (subscribe failed)**

Change:

```rust
                web_sys::console::error_1(&wasm_bindgen::JsValue::from_str(&format!(
                    "RelaySignaler subscribe: {e}"
                )));
```

to:

```rust
                tracing::error!(error = %e, "RelaySignaler subscribe");
```

- [ ] **Step 12: Replace `relay_signaler.rs:177-179` (event error)**

Change:

```rust
                    web_sys::console::error_1(&wasm_bindgen::JsValue::from_str(&format!(
                        "RelaySignaler event: {e}"
                    )));
```

to:

```rust
                    tracing::error!(error = %e, "RelaySignaler event");
```

- [ ] **Step 13: Replace `relay_signaler.rs:184-186` (handle_entry failed)**

Change:

```rust
                web_sys::console::warn_1(&wasm_bindgen::JsValue::from_str(&format!(
                    "RelaySignaler handle_entry: {e}"
                )));
```

to:

```rust
                tracing::warn!(error = %e, "RelaySignaler handle_entry");
```

- [ ] **Step 14: Replace `voice.rs:72-74` (decode failed)**

Change:

```rust
                    web_sys::console::warn_1(
                        &format!("sunset-voice: decode failed for one frame: {e}").into(),
                    );
```

to:

```rust
                    tracing::warn!(error = %e, "decode failed for one frame");
```

(Drop the `sunset-voice:` prefix — module path covers it.)

- [ ] **Step 15: Build native + WASM**

Run: `nix develop --command cargo build -p sunset-web-wasm --all-features`
Expected: `Finished` with no warnings.

Run: `nix develop --command cargo build -p sunset-web-wasm --target wasm32-unknown-unknown`
Expected: `Finished` with no warnings.

- [ ] **Step 16: Clippy**

Run: `nix develop --command cargo clippy -p sunset-web-wasm --all-features --all-targets -- -D warnings`
Expected: `Finished` with no warnings.

- [ ] **Step 17: Confirm no `web_sys::console` remains in `sunset-web-wasm`**

Run: `nix develop --command bash -c "rg -n 'web_sys::console' crates/sunset-web-wasm/ || echo OK"`
Expected: prints `OK`.

- [ ] **Step 18: Commit**

```bash
git add crates/sunset-web-wasm/Cargo.toml crates/sunset-web-wasm/src/client.rs crates/sunset-web-wasm/src/presence_publisher.rs crates/sunset-web-wasm/src/relay_signaler.rs crates/sunset-web-wasm/src/voice.rs
git commit -m "$(cat <<'EOF'
sunset-web-wasm: route web logs through tracing

Replaces twelve web_sys::console::{error,warn}_1 sites across client.rs,
presence_publisher.rs, relay_signaler.rs, and voice.rs with
tracing::{error,warn}!. Updates the stale doc comment in client.rs:477.
Adds tracing workspace dep. Subscriber wiring follows in the next commit.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 6: Install the WASM tracing subscriber

**Files:**
- Modify: `crates/sunset-web-wasm/Cargo.toml`
- Modify: `crates/sunset-web-wasm/src/lib.rs`

- [ ] **Step 1: Add `wasm-tracing` as a wasm-only dep on `sunset-web-wasm`**

In `crates/sunset-web-wasm/Cargo.toml`, find the existing `[target.'cfg(target_arch = "wasm32")'.dependencies]` block. Add (alphabetical position):

```toml
wasm-tracing.workspace = true
```

For example, place it between `wasm-bindgen-futures.workspace = true` and `wasmtimer.workspace = true`. The block should now contain (in order): `getrandom_02`, `js-sys`, `wasm-bindgen`, `wasm-bindgen-futures`, `wasm-tracing`, `wasmtimer`, `web-sys`.

- [ ] **Step 2: Add the global init in `lib.rs`**

The current `crates/sunset-web-wasm/src/lib.rs` is a list of `#[cfg(target_arch = "wasm32")] mod …;` declarations followed by re-exports. Append the start function at the end of the file (after the last `pub use` and the non-wasm fallbacks):

```rust
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen(start)]
pub fn __sunset_web_wasm_start() {
    let mut config = wasm_tracing::WasmLayerConfig::default();
    config.set_max_level(tracing::Level::INFO);
    // The Result is `Err` only if a global subscriber was already set,
    // which can't happen here: this function is the sole #[wasm_bindgen(start)]
    // entrypoint and runs exactly once per module load.
    let _ = wasm_tracing::set_as_global_default_with_config(config);
}
```

(`set_as_global_default_with_config` returns `Result<(), SetGlobalDefaultError>` and the workspace lint `unused_must_use = deny` would reject ignoring it implicitly — hence the explicit `let _ = …`. Using the fully-qualified `wasm_bindgen::prelude::wasm_bindgen(start)` avoids needing a `use` import in `lib.rs`. The function name is prefixed with `__` to flag it as an internal entrypoint that JS shouldn't call.)

- [ ] **Step 3: Build for native to confirm the cfg gate is correct**

Run: `nix develop --command cargo build -p sunset-web-wasm --all-features`
Expected: `Finished` with no warnings. The new function and the new dep are both behind `cfg(target_arch = "wasm32")`, so native builds skip them entirely.

- [ ] **Step 4: Build for WASM**

Run: `nix develop --command cargo build -p sunset-web-wasm --target wasm32-unknown-unknown`
Expected: `Finished` with no warnings. `Cargo.lock` should now show `wasm-tracing` actually being pulled into the dependency graph.

- [ ] **Step 5: Clippy**

Run: `nix develop --command cargo clippy -p sunset-web-wasm --all-features --all-targets -- -D warnings`
Expected: `Finished` with no warnings.

- [ ] **Step 6: Commit**

```bash
git add crates/sunset-web-wasm/Cargo.toml crates/sunset-web-wasm/src/lib.rs Cargo.lock
git commit -m "$(cat <<'EOF'
sunset-web-wasm: install the WASM tracing subscriber

#[wasm_bindgen(start)] now installs wasm-tracing's WasmLayer at module
load, default level INFO, forwarding to console.{log,warn,error}. JS
that calls Client::new now sees logs immediately in browser devtools
without any additional setup.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 7: Fix the duplicate banner emit in `sunset-relay`

**Files:**
- Modify: `crates/sunset-relay/src/relay.rs` (lines 127-128)
- Modify: `crates/sunset-relay/src/main.rs` (line 23)

- [ ] **Step 1: Remove the redundant `tracing::info!` banner line**

In `crates/sunset-relay/src/relay.rs`, find lines 127-128:

```rust
        tracing::info!("\n{}", banner);
        println!("{}", banner);
```

Remove the `tracing::info!` line, keeping only:

```rust
        println!("{}", banner);
```

(Per spec: the banner is operator-facing TTY output, not a log line. Logs go through `tracing`; banner stays a `println!`.)

- [ ] **Step 2: Bump the default `EnvFilter` to keep `sunset_sync` info visible**

In `crates/sunset-relay/src/main.rs` line 23:

```rust
                .unwrap_or_else(|_| EnvFilter::new("sunset_relay=info,sunset_sync=warn")),
```

Change to:

```rust
                .unwrap_or_else(|_| EnvFilter::new("sunset_relay=info,sunset_sync=info")),
```

(Why: Task 2 routed lifecycle events like "peer disconnected" through `tracing::info!`. Before this change those went through `eprintln!` and were always visible. Operators running the relay binary would lose them under the existing `sunset_sync=warn` default. Bumping to `info` preserves the prior surface; operators who want less can set `RUST_LOG=sunset_sync=warn` themselves.)

- [ ] **Step 3: Build and test the relay**

Run: `nix develop --command cargo build -p sunset-relay --all-features`
Expected: `Finished` with no warnings.

Run: `nix develop --command cargo test -p sunset-relay --all-features`
Expected: all tests pass.

- [ ] **Step 4: Clippy**

Run: `nix develop --command cargo clippy -p sunset-relay --all-features --all-targets -- -D warnings`
Expected: `Finished` with no warnings.

- [ ] **Step 5: Commit**

```bash
git add crates/sunset-relay/src/relay.rs crates/sunset-relay/src/main.rs
git commit -m "$(cat <<'EOF'
sunset-relay: deduplicate banner emit; bump default sync filter to info

The banner was being printed twice — once via tracing::info! and once
via println! — because both routes are live by default. Drop the
tracing one; the banner is operator TTY output, not a log line.

Also bump the default EnvFilter to sunset_sync=info so operators still
see the lifecycle lines (e.g. "peer disconnected") that the engine.rs
migration moved from unconditional eprintln! to tracing::info!.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 8: Drop the now-unused `web-sys` `"console"` feature

**Files:**
- Modify: `Cargo.toml` (workspace-root `web-sys` features list)
- Modify: `crates/sunset-web-wasm/Cargo.toml` (crate-level `web-sys` features list)

- [ ] **Step 1: Remove `"console"` from the workspace `web-sys` features**

In the workspace `Cargo.toml`, find the `web-sys = { version = "0.3", features = [ … ] }` line in `[workspace.dependencies]`. Remove the `"console"` entry. The features list before:

```toml
web-sys = { version = "0.3", features = [
  "WebSocket",
  "MessageEvent",
  "BinaryType",
  "CloseEvent",
  "Event",
  "console",
  "RtcPeerConnection",
  …
```

After:

```toml
web-sys = { version = "0.3", features = [
  "WebSocket",
  "MessageEvent",
  "BinaryType",
  "CloseEvent",
  "Event",
  "RtcPeerConnection",
  …
```

(Leave every other entry untouched.)

- [ ] **Step 2: Remove `"console"` from `sunset-web-wasm`'s crate-level web-sys features**

In `crates/sunset-web-wasm/Cargo.toml`, find:

```toml
web-sys = { workspace = true, features = ["WebSocket", "MessageEvent", "BinaryType", "CloseEvent", "Event", "console"] }
```

Change to:

```toml
web-sys = { workspace = true, features = ["WebSocket", "MessageEvent", "BinaryType", "CloseEvent", "Event"] }
```

- [ ] **Step 3: Build native + WASM to confirm nothing broke**

Run: `nix develop --command cargo build --workspace --all-features`
Expected: `Finished` with no warnings.

Run: `nix develop --command cargo build -p sunset-web-wasm --target wasm32-unknown-unknown`
Expected: `Finished` with no warnings.

(If either fails with an error mentioning `web_sys::console::*` — something in the codebase still uses the feature. That would indicate a missed conversion in Tasks 4 or 5; fix it there rather than re-adding the feature.)

- [ ] **Step 4: Clippy**

Run: `nix develop --command cargo clippy --workspace --all-features --all-targets -- -D warnings`
Expected: `Finished` with no warnings.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml crates/sunset-web-wasm/Cargo.toml Cargo.lock
git commit -m "$(cat <<'EOF'
deps: drop unused web-sys "console" feature

After the tracing migration, no crate calls web_sys::console::* anymore
— wasm-tracing's WasmLayer accesses the console internally. Trim the
feature from both the workspace dep list and sunset-web-wasm's
crate-level override.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 9: Final verification

This task is a single batched check, no commits.

- [ ] **Step 1: Full workspace build (native)**

Run: `nix develop --command cargo build --workspace --all-features`
Expected: `Finished` with no warnings.

- [ ] **Step 2: Full workspace build (WASM)**

Run: `nix develop --command cargo build -p sunset-web-wasm --target wasm32-unknown-unknown`
Expected: `Finished` with no warnings.

- [ ] **Step 3: Full workspace test**

Run: `nix develop --command cargo test --workspace --all-features`
Expected: all tests pass.

- [ ] **Step 4: Full workspace clippy**

Run: `nix develop --command cargo clippy --workspace --all-features --all-targets -- -D warnings`
Expected: `Finished` with no warnings.

- [ ] **Step 5: Format check**

Run: `nix develop --command cargo fmt --all --check`
Expected: no diff. If it diffs, run `nix develop --command cargo fmt --all` and amend the relevant earlier commit (or land a tiny "fmt" commit on top — talk to the user about which they prefer).

- [ ] **Step 6: Grep gate — `eprintln!` should be gone**

Run: `nix develop --command bash -c "rg -n 'eprintln!' crates/ || echo OK"`
Expected: prints `OK`.

- [ ] **Step 7: Grep gate — `web_sys::console::{error,warn}` should be gone**

Run: `nix develop --command bash -c "rg -n 'web_sys::console::(error|warn)' crates/ || echo OK"`
Expected: prints `OK`.

- [ ] **Step 8: Grep gate — banner `println!` should still be present**

Run: `nix develop --command bash -c "rg -n 'println!' crates/"`
Expected: a single hit, `crates/sunset-relay/src/relay.rs:128: println!("{}", banner);` (line number may shift). Anything else is a regression.

- [ ] **Step 9: Manual smoke (operator TTY)**

Run the relay binary briefly:

```
nix develop --command cargo run -p sunset-relay
```

Expected at startup: the banner appears exactly once on stdout (preceded by the `tracing-subscriber` fmt output for the binary's own info lines). Hit Ctrl+C; the shutdown line "received Ctrl+C, shutting down" should appear once.

- [ ] **Step 10: Manual smoke (browser console)**

Build and serve the web app per the project's normal workflow (the `web/` directory has its dev script). Open the browser devtools console, then induce a recoverable error path — easiest is to point the client at a relay URL that doesn't resolve. Confirm:

  - The error message appears in the browser console.
  - It's tagged at the level the converted call uses (red for `error`, yellow for `warn`).
  - The module path appears in the line (e.g. `sunset_web_wasm::client`), proving the subscriber is installed and the `target = module_path!()` mapping is active.

If logs do not appear at all: the `#[wasm_bindgen(start)]` may not have run. Check the browser console for any panic during module load, and verify the wasm bundle contains the `__sunset_web_wasm_start` symbol.

- [ ] **Step 11: Report ready**

If all of the above pass, the migration is verified end-to-end. Push the branch and open a PR via `gh pr create` (per project workflow rule — use `gh`, not the API).
