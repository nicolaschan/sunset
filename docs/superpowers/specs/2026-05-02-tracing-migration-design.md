# Tracing Migration Design

**Date:** 2026-05-02
**Scope:** Replace ad-hoc logging (`eprintln!`, `web_sys::console::*`) across the workspace with the `tracing` facade so every module emits through one structured pipeline. Wire a WASM subscriber so browser consumers continue to see warnings/errors in devtools. Fix the duplicate banner emit in `sunset-relay`.
**Out of scope:** new `#[instrument]` spans, structured-field migrations beyond the mechanical rewrite, test-time tracing (`tracing-test`), runtime-tunable WASM filters, log lines in crates that don't yet emit (`sunset-store*`, etc.), and any change to `sunset-relay`'s already-converted call sites.

## Goal

`sunset-relay` already emits via `tracing::*` and initializes a `tracing-subscriber` with `EnvFilter` in `main.rs`. Everything below it still uses one of three ad-hoc mechanisms:

- `eprintln!` — `sunset-sync/src/engine.rs` (8 sites), `sunset-core/src/membership.rs` (2 sites). Compiles for both native and `wasm32-unknown-unknown`. On WASM, `eprintln!` writes to a stderr that's effectively invisible in the browser.
- `web_sys::console::error_1` / `warn_1` — `sunset-web-wasm/{client,presence_publisher,relay_signaler,voice}.rs` (~12 sites), `sunset-sync-webrtc-browser/src/wasm.rs` (1 site). Native consumers can't observe these at all.
- One `println!` paired with a `tracing::info!` for the relay startup banner — see "Banner duplication" below.

The result is that operators running the native relay see a coherent log stream, but anyone debugging the web client either gets nothing (sync engine on WASM swallows `eprintln!`) or has to know that some events go through `console.error` and others don't go anywhere. Unifying on `tracing` gives every host one knob to turn (`RUST_LOG` natively, a build-time level on WASM) and lets future code add `#[instrument]` spans without first having to migrate the call site away from `eprintln!`.

## Architecture

```
                       tracing facade (workspace dep)
                              │
        ┌─────────────────────┼──────────────────────┐
        │                     │                      │
   native bins           wasm32 (browser)       (no subscriber → silent)
        │                     │                      │
   tracing-subscriber     wasm-tracing             unit tests
   EnvFilter "info"       WASMLayer / Level::INFO
   fmt to stderr          forwards to console.{log,warn,error}
```

`tracing-subscriber` is already a `sunset-relay`-only dep and stays that way. `wasm-tracing` (the actively-maintained fork of `tracing-wasm`, currently `2.x`) is added as a wasm-only dep on `sunset-web-wasm`, which is the only crate with a wasm entrypoint, so it's also the only place we initialize a global subscriber. Library crates (`sunset-sync`, `sunset-core`, `sunset-sync-webrtc-browser`) only add `tracing` itself — never a subscriber — so they remain composable.

### Why `wasm-tracing` (Option A) over a hand-rolled `MakeWriter`

`tracing-subscriber`'s `fmt` layer assumes a stdout/stderr writer; on `wasm32-unknown-unknown` that means a writer the user never sees. `wasm-tracing` provides a `WasmLayer` that dispatches each event directly to `console.log`/`warn`/`error` based on level, with optional `console.time`/`timeEnd` integration for spans. That's exactly the shape we need for a browser. `wasm-tracing` does pull `tracing-subscriber` transitively (its registry is what hosts the layer), but we don't have to enable the heavyweight `env-filter` or `fmt` features in our build — only the registry is required, and that's the default. The day we want runtime-tunable filters in the browser we can revisit; today's status quo (no filtering) is what the existing `console::*` calls already do.

### Why initialize via `#[wasm_bindgen(start)]`

`Client::new` runs every time JS constructs a client, but a tracing subscriber must be set globally exactly once — `set_as_global_default` errors on the second call. Putting initialization in `Client::new` would either crash on a second `Client` or require us to gate the call ourselves with a `Once`. `#[wasm_bindgen(start)]` runs once when the module loads, before any JS-callable function, which is the right shape: by the time `Client::new` runs, logging is already live.

## Scope of Conversion

### Crates that gain a `tracing` workspace dep

- `sunset-sync` — for `engine.rs`.
- `sunset-core` — for `membership.rs`.
- `sunset-sync-webrtc-browser` — for `wasm.rs`.
- `sunset-web-wasm` — for `client.rs`, `presence_publisher.rs`, `relay_signaler.rs`, `voice.rs`. Also gains `wasm-tracing` under `[target.'cfg(target_arch = "wasm32")'.dependencies]` and a `lib.rs` start hook.

`sunset-relay` already has both `tracing` and `tracing-subscriber`; no `Cargo.toml` change there.

### Call-site mapping rules

The conversion is mechanical. Every existing log line gets remapped according to this table:

| Source form | Target | Reasoning |
|---|---|---|
| `web_sys::console::error_1(…)` | `tracing::error!(…)` | One-to-one severity match. |
| `web_sys::console::warn_1(…)` | `tracing::warn!(…)` | One-to-one. |
| `eprintln!("...err…")` for a swallowed/recovered error | `tracing::warn!(…)` | The error is being recovered; warn is the right severity. Promoting to `error!` would imply we want pager-style attention. |
| `eprintln!` for an operational lifecycle event (peer connect/disconnect, replay started/finished) | `tracing::info!(…)` | These are interesting at the default `info` filter; demoting them would hide the "peer X disconnected" line that operators actually want. |
| `eprintln!` for adversary-controlled / high-volume conditions (e.g. "dropping ephemeral datagram — bad signature") | `tracing::debug!(…)` | An attacker can drive these at line rate; at `info` they'd flood logs. Stays observable when an operator turns up `RUST_LOG=…=debug`. |
| `format!("crate-name: …")` prefix inside the message string | drop the prefix | `tracing` records `target = module_path!()` automatically; double-prefixing is noise. |
| `format!("{e}")` of an error in the message | move to a structured field: `tracing::warn!(error = %e, "…")` | Keeps the human-readable message stable while making the error available to structured consumers. Other ad-hoc fields (peer ids, conn ids) follow the same `field = …` form when they're already in the call. |

The exact level for each existing site (so the implementation plan has zero ambiguity):

**`sunset-sync/src/engine.rs`**
- `:403` "transport accept failed; continuing" → `warn`
- `:411` "…" (next eprintln) → `warn` (same recovery context)
- `:596` "peer disconnected" → `info`
- `:629` "replay_existing_subscriptions" failure → `warn`
- `:637` "replay_existing_subscriptions" failure → `warn`
- `:723` "digest scan failed" → `warn`
- `:799` "…" → assess at implementation time using the table; if it's a swallowed error, `warn`; if lifecycle, `info`. The plan author should not guess from a line number alone.
- `:974` "dropping ephemeral datagram — bad signature" → `debug`

**`sunset-core/src/membership.rs`**
- `:287` "presence subscribe failed" → `warn`
- `:305` "presence event: {e}" → `warn`

**`sunset-web-wasm`** — every `console::error_1` becomes `tracing::error!`; every `console::warn_1` becomes `tracing::warn!`. No level reinterpretation: we trust the original author's pick of `error_1` vs `warn_1`. Sites:

- `client.rs:98` "sync engine exited: {e}" → `error`
- `client.rs:394` "store.subscribe: {e}" → `error`
- `client.rs:407` "store event: {e}" → `error`
- `client.rs:416` "get_content: {e}" → `error`
- `client.rs:424` (multi-line) → `error`
- `client.rs:492` (multi-line) → `error`
- `client.rs:499` (multi-line) → `error`
- `presence_publisher.rs:30` "presence publisher: {e}" → `warn`
- `relay_signaler.rs:165` (multi-line) → `error`
- `relay_signaler.rs:177` (multi-line) → `error`
- `relay_signaler.rs:184` (multi-line) → `warn`
- `voice.rs:72` (multi-line) → `warn`

Also: the doc comment in `client.rs:477` ("Errors are logged via `web_sys::console`…") needs an update — replace `web_sys::console` with `tracing` so the doc stays accurate.

**`sunset-sync-webrtc-browser/src/wasm.rs:421`** — `console::warn_1` → `tracing::warn!`.

### Banner duplication in `sunset-relay/src/relay.rs`

Lines 127–128 today:

```rust
tracing::info!("\n{}", banner);
println!("{}", banner);
```

If `RUST_LOG=info` (or no `RUST_LOG`, since the relay defaults to info), the operator sees the banner twice. The fix is to remove the `tracing::info!` line and keep the `println!`. The banner is startup UX — an operator who runs the binary expects to see it regardless of log filter, and gating it on `RUST_LOG` would hide it from anyone who narrows the filter (e.g. `RUST_LOG=warn`). Logs and TTY output have different audiences; the banner belongs to the latter.

### `web-sys` features

After the conversion, check whether anything in `sunset-web-wasm` still needs `web-sys`'s `"console"` feature. If not, drop it from the feature list in `crates/sunset-web-wasm/Cargo.toml` to keep the WASM build's surface tight.

## Initialization

### `sunset-relay`

Already correct in `crates/sunset-relay/src/main.rs`:

```rust
tracing_subscriber::fmt()
    .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
    .init();
```

Unchanged.

### `sunset-web-wasm`

Add to `crates/sunset-web-wasm/src/lib.rs`:

```rust
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen(start)]
pub fn __sunset_web_wasm_start() {
    let mut config = wasm_tracing::WasmLayerConfig::default();
    config.set_max_level(tracing::Level::INFO);
    // The Result is `Err` only if a global subscriber was already set,
    // which can't happen here: this function is the sole #[wasm_bindgen(start)]
    // entrypoint and runs exactly once per module load.
    let _ = wasm_tracing::set_as_global_default_with_config(config);
}
```

(API per `wasm-tracing` 2.1.0: `set_as_global_default_with_config(WasmLayerConfig)` returns `Result<(), SetGlobalDefaultError>`; the workspace lint `unused_must_use = deny` requires the explicit `let _ =`. The implementation plan pins the exact version.) `wasm_bindgen(start)` guarantees one call per module load — by the time `Client::new` runs, the subscriber is live.

### Workspace `Cargo.toml`

Add `wasm-tracing` to the `[workspace.dependencies]` block alongside the existing `tracing` line so the version is pinned in one place. The crate is already wasm-only by construction, so no `target.cfg` is needed at the workspace level — only `sunset-web-wasm` references it, gated by `target_arch = "wasm32"`.

## Tests

No new test infrastructure. Library-crate unit tests run without a global subscriber, which makes `tracing::*` calls into no-ops — same observable behavior as the current `eprintln!` calls were already getting (cargo test captures stderr by default). The conformance suite and integration tests don't assert on log output and never have, so the migration is silent for them.

If a future test wants to assert on a log line, it can opt in to `tracing-subscriber::fmt::test` or `tracing-test` — out of scope here.

## Verification

A reviewer should be able to confirm the migration by running:

```
nix develop --command cargo build --workspace --all-features
nix develop --command cargo build -p sunset-web-wasm --target wasm32-unknown-unknown
nix develop --command cargo test --workspace --all-features
nix develop --command cargo clippy --workspace --all-features --all-targets -- -D warnings
nix develop --command cargo fmt --all --check
```

Plus a manual smoke test: build and serve the web app, open devtools, induce a recoverable error path (e.g. point the client at a relay URL that doesn't resolve), and confirm the message lands in the browser console at the right level (warn or error). Also run the relay binary locally and confirm the startup banner appears exactly once.

A grep gate at the end of the work: `rg -n 'eprintln!|web_sys::console::(error|warn)' crates/` should return zero hits in the converted files. Banner-style `println!` in `sunset-relay/src/relay.rs` is the single legitimate exception and should still be present.

## Risks and Mitigations

- **WASM subscriber can't be re-initialized.** Calling `set_as_global_default` twice errors. Mitigation: single `#[wasm_bindgen(start)]` entry; no other code path initializes.
- **Level drift in `engine.rs:799`.** That line is ambiguous from the spec — the implementer must read the surrounding context and apply the level-mapping table. The plan should call this out as a "look at the code" step rather than a hard-coded mapping.
- **Bundle-size impact on WASM.** `wasm-tracing` is small (single layer, no `fmt`), but should be measured during plan execution. If the delta is unexpected, fall back to a tiny custom layer (Option C from brainstorming). Not anticipated.
- **Hermeticity.** `wasm-tracing` is a Cargo dep, not a system dep, so it does not need a `flake.nix` entry. The flake's existing Rust toolchain + wasm target already covers the build.
