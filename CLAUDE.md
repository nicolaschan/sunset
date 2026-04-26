# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Dev shell

Toolchain is pinned by `flake.nix` (Rust stable + `wasm32-unknown-unknown` target, `cargo-watch`, `cargo-nextest`). `.envrc` uses `use flake`, so direnv-allowed shells get tools automatically. Otherwise, prefix cargo commands with `nix develop --command`:

```
nix develop --command cargo test --workspace --all-features
nix develop --command cargo clippy --workspace --all-features --all-targets -- -D warnings
nix develop --command cargo fmt --all --check
```

### Hermeticity rule

**Every dependency goes through the flake.** Don't reach for system Rust, system cargo, or globally-installed tools — if a build or test step needs something, add it to `flake.nix`'s `buildInputs` (or `devShells`/`packages`/`apps` as appropriate). The repository must `nix develop` / `nix build` / `nix run` cleanly on any NixOS machine with no implicit dependencies on local filesystem state, environment variables, or pre-installed tooling. If you find yourself writing setup instructions like "first install X" or "make sure Y is on PATH," that's a flake gap to close, not a docs item.

Run a single test: `cargo test -p sunset-store-memory store::tests::insert_replaces_with_higher_priority`.

The conformance suite lives at `crates/sunset-store/src/test_helpers.rs` behind feature `test-helpers`. Backends drive it via an integration test (e.g. `crates/sunset-store-memory/tests/conformance.rs`); use `--all-features` so the gate flips on.

Workspace lints (`Cargo.toml`): `unsafe_code = deny`, `unused_must_use = deny`, plus a few clippy warnings. New crates should set `[lints] workspace = true`.

## What this repo is

sunset.chat — a peer-to-peer E2E-encrypted chat app with multiple client surfaces (web/Gleam, native TUI, Minecraft mod, Docker relay) all sharing a single Rust core that compiles to both native and WASM. Authoritative design docs:

- `docs/superpowers/specs/2026-04-25-sunset-chat-architecture-design.md` — system architecture (north-star)
- `docs/superpowers/specs/2026-04-25-sunset-store-and-sync-design.md` — combined sunset-store + sunset-sync subsystem design
- `docs/superpowers/plans/2026-04-25-sunset-store-core-and-memory-backend.md` — Plan 1 (the implementation plan that produced the current code)

When the spec and the code disagree, the spec wins; either fix the code or amend the spec with a marked revision.

## Layered architecture

```
Application (chat semantics, identity)
    ↓
Sync (sunset-sync — interest-set replication)        [not yet implemented]
    ↓
Store (sunset-store + backends — signed CRDT KV +    [Plan 1 complete: memory backend]
       content-addressed blobs)                      [pending: -fs, -indexeddb backends]
    ↓
Crypto / Transport
```

Currently merged: `sunset-store` (trait + types + conformance suite) and `sunset-store-memory` (in-memory backend). The remaining backends (`sunset-store-fs`, `sunset-store-indexeddb`), `sunset-sync`, `sunset-core`, and the per-host crates (`sunset-tui`, `sunset-relay`, `sunset-web`, `sunset-mod`) are deferred to future plans, each going through its own writing-plans cycle before implementation.

## Store contract (load-bearing)

Every backend implements `sunset_store::Store` and must:

- **Verify on insert.** Call the configured `SignatureVerifier` (no built-in scheme — verifier is host-supplied). The store crate itself doesn't know about Ed25519 / ML-DSA / etc.
- **LWW by priority** on `(verifying_key, name)`. Higher priority wins; equal-or-lower priority returns `Error::Stale`. **No tombstones** — TTL pruning + GC handle removal.
- **Hash match** when an insert supplies a blob: `entry.value_hash == blob.hash()` or `Error::HashMismatch`.
- **Atomic per-entry writes** of `(blob, entry)`. No batch atomicity across multiple entries.
- **Lazy dangling refs** allowed: an entry whose blob isn't local yet is fine; sync fetches it later.
- **Content-addressed blob store** with mark-and-sweep GC over reachable refs (DAG via `ContentBlock.references`).
- **Monotonic per-store sequence** for cursors. `current_cursor()` returns the *next-to-be-assigned* sequence; `Replay::Since(c)` matches `sequence >= c.0`.

Subscription event ordering (matters for tests): on a write, `Inserted`/`Replaced`/`Expired` fire first, then `BlobAdded`/`BlobRemoved`. `BlobAdded`/`BlobRemoved` are delivered to **all** subscribers regardless of filter (they have no key to match on).

Wire format is **postcard** (`bincode` is deprecated; do not reintroduce). The v1 encoding is frozen — there's a hex-pinned test vector for `ContentBlock::hash()` in `types.rs` to detect accidental wire-format drift.

## WASM compatibility constraints

The store and everything below the application layer must compile to `wasm32-unknown-unknown`. That shapes the API:

- `#[async_trait(?Send)]` on the `Store` trait — backends are `?Send`.
- Streams are `futures::stream::LocalBoxStream` (not `BoxStream`); use `async_stream::stream!` to build them.
- Don't reach for `tokio::spawn` / multi-threaded executors in the data plane — tokio's sync primitives (`Mutex`, channels) are fine, but assume single-threaded WASM.
- Don't introduce `Send + Sync` bounds on data-plane types unless there's a concrete reason. (Exception: `SignatureVerifier` is `Send + Sync` so a single verifier instance can be shared.)

## Subscription invariants in `sunset-store-memory`

Two non-obvious correctness rules that exist because the wrong shape is easy to write:

1. **Broadcast happens inside the inner `Mutex<Inner>` critical section** (in `insert`, `delete_expired`, `gc_blobs`), and `subscribe` registers its `Weak<Subscription>` inside the same critical section. This is what serializes "history snapshot vs. live channel" and prevents an event from being delivered both ways or neither way. Don't move broadcasts outside the lock.
2. **Per-subscription channel must stay unbounded** (`mpsc::UnboundedSender`). Because `broadcast` runs under the inner mutex, switching to a bounded channel would let a slow subscriber stall every writer. The invariant is documented in `subscription.rs`; preserve it.

## Workflow notes

- Worktrees live under `.worktrees/` (gitignored). Plans are typically executed via the superpowers subagent-driven-development flow with two-stage review (spec compliance → code quality).
- Cargo.lock is committed (workspace will eventually ship binaries — relay, TUI).
- Conventional commits aren't enforced; recent history uses imperative, scope-prefixed messages without a `Co-Authored-By` trailer on this branch.
