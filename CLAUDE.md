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

Workspace lints (`Cargo.toml`): `unsafe_code = deny`, `unused_must_use = deny`, plus the workspace clippy policy. New crates should set `[lints] workspace = true`.

### Clippy policy: no suppressions

Every clippy warning must be fixed at the source. **`#[allow(clippy::...)]` and `#[expect(clippy::...)]` are forbidden in our source.** This is enforced two ways:

1. `cargo clippy --workspace --all-features --all-targets -- -D warnings` runs in CI (`.github/workflows/test.yml`) and fails the build on any clippy warning.
2. `scripts/check-no-clippy-allow.sh` greps `crates/` for clippy suppressions and fails CI if any are found. It only scans our source — `#[allow]`s emitted by macro expansions inside dependencies (clap, tokio, etc.) are unaffected.

If clippy flags something, **fix the root cause**: refactor signatures (e.g. bundle args into a struct to drop below `too_many_arguments`), pick a different primitive (e.g. `tokio::sync::Mutex` instead of `RefCell` to avoid `await_holding_refcell_ref`), or rename the API (e.g. constructors that don't return `Self` should be named `start` / `open` / `connect`, not `new`). Do not suppress.

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

- **Use git worktrees for all implementation work.** Use the `superpowers:using-git-worktrees` skill before starting any feature or plan execution. Worktrees live under `.worktrees/` (gitignored). Plans are executed via the superpowers subagent-driven-development flow with two-stage review (spec compliance → code quality).
- Cargo.lock is committed (workspace will eventually ship binaries — relay, TUI).
- Commits must include a `Co-Authored-By` trailer naming the actual model used (e.g. `Co-Authored-By: Claude Sonnet 4.6 <noreply@anthropic.com>` when running Sonnet 4.6, `Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>` when running Opus 4.7).
- Use `gh pr create` (GitHub CLI) to open pull requests — not the API or any other method.

## Debugging discipline

**Tests encode the contract — don't patch around them.**

Before touching a failing test, evaluate it from UX and API-contract perspectives: *should* this work? If yes, the test is correct and the bug is elsewhere. Never change, disable, or increase timeouts on a test to make it pass when the underlying behavior would be unacceptable to a real user. A one-minute connection delay is not an acceptable UX; if an integration test times out in 30 s under those conditions, the timeout is right and the code is broken.

If a unit test requires workarounds for edge cases a caller would never expect to hit, that signals an API design problem — fix the API, not the test.

**You cannot fix a correct test by modifying the test.** A test is correct iff it expresses what a real API user would write and expect. If a correct test fails, the bug is in the code under test — full stop. Modifying a correct test to make it pass is *camouflage*, not a fix; the bug still ships, and the next caller hits it. The following ALL count as patching the test, not fixing the bug:

- Adding `wait_for(...)` polls on engine-internal state (e.g. registries, queues, handshake completion) so a basic API call works "deterministically".
- Calling test-only inspector methods (`knows_*`, `has_*`, `is_*_complete`) to gate a user-level action that the documented API doesn't ask the user to gate.
- Inserting `tokio::time::sleep` to mask a race.
- Adding extra `assert!` checks for engine-internal state framed as "hardening preconditions" — a real user has no way to establish those preconditions, so the test was never racy from the user's perspective; the *engine* was racy.
- Replacing the user's straight-line action sequence with a sequence of waits + checks the user couldn't perform.

Litmus test: *would a real caller, reading only the public API docs, write the test the new way?* If no, you're patching the test.

**Decision flow when a test fails:**

1. Read the test as if you were the API's user. Ignore implementation. What does it claim the system does?
2. Would a real user, reading only the public API, reasonably write this test and expect it to pass? If yes, the test is correct.
3. The fix is in the code under test, not in the test.
4. If the fix is architectural (new abstractions, protocol changes, contract revisions, public API semantics), stop and **propose the fix to the user explicitly** via `superpowers:brainstorming` / `superpowers:writing-plans`. *Do not silently fall back to a test-side workaround "because it's lower-risk" or "needs user confirmation per rule 3."* "Needs confirmation" means **ask**, not retreat.
5. Lower-risk-to-CI ≠ lower-risk-to-users. A test workaround that hides a real race is *higher* risk: the race still fires in production, just without a tripwire.

**Forbidden rationalizations** — if you catch yourself thinking these, stop and re-read this section:

- "The test is just making preconditions deterministic." → If the API contract requires a precondition the user can't establish, the API is broken.
- "It's lower-risk to fix the test." → Risk to whom? Test-side patches keep the bug live for users.
- "The architectural fix needs user confirmation, so I'll do the test-side fix in the meantime." → Then *ask* about the architectural fix. Do not paper over.
- "I'll note the engine race in the commit/PR description as a follow-up." → Follow-ups don't get done; the bug ships.
- "Adding a `wait_for` isn't disabling or increasing the test's timeout." → It's adding setup the user can't perform. Same category.

**Debugging process:**

1. Use the `superpowers:systematic-debugging` skill to understand the bug before proposing any fix.
2. Step back from the immediate symptom and reason about how the broader architecture *should* solve the problem.
3. If the fix requires architectural decisions (new abstractions, protocol changes, contract revisions), stop and confirm with the user via the `superpowers:brainstorming` and `superpowers:writing-plans` skills before writing any code. Confirming means proposing the architectural change and asking; it does **not** mean choosing the test-side workaround as a default.
