# Code Fix — Iteration 1

## Cluster addressed
**Broadcast call-site duplication preserved across backends**
Why this cluster: top of recommended address order, high-confidence.

## Root cause identified
The previous commit unified `SubscriptionList` itself but left both backends' `insert` methods hand-orchestrating the broadcast ordering: first the entry event (`Inserted`/`Replaced`), then optionally `BlobAdded`. Two copies of a load-bearing project invariant ("entry events before blob events", per project CLAUDE.md), enforced by hand.

## Structural shape chosen
Add `SubscriptionList::publish_insert(entry_event: &Event, blob_added: Option<Hash>)` that encapsulates the ordering. Both backends construct the entry event locally (their storage shapes differ — `MemoryStore` has `Option<SignedKvEntry>` for the prior entry, `FsStore` has `kv::InsertOutcome`) and hand it off in one call. The ordering rule lives in one place; backends cannot forget it.

## What was changed
- `crates/sunset-store/src/subscription.rs` — added `Hash` import; added `publish_insert(&self, entry_event: &Event, blob_added: Option<Hash>)` method. `broadcast` itself is unchanged (the related variant-enumeration concern is a separate cluster, deliberately not addressed in this iteration).
- `crates/sunset-store-memory/src/store.rs` — `insert` constructs the entry event locally then calls `publish_insert`. 10 lines collapsed to 6.
- `crates/sunset-store-fs/src/store.rs` — same pattern. 16 lines collapsed to 11.

## What was deliberately not addressed
- Cluster 2 (Filter::matches_event extraction) — separate iteration.
- Standalone: redundant `tokio.workspace = true` in `[dev-dependencies]` of `sunset-store/Cargo.toml`.
- Standalone: load-bearing doc comment on `SubscriptionList::entries` Mutex unwrap (borderline / allowed).
- Standalone: no direct unit tests for `SubscriptionList`.

## Concerns surfaced during the fix
- An intermediate version of the fix took `entry_event` by value, but clippy's `needless_pass_by_value` flagged it (the value is only used by reference inside, since `broadcast` takes `&Event`). Fixed to take by reference.

## Verification
- `cargo clippy --workspace --all-features --all-targets -- -D warnings`: pass
- `cargo test --workspace --all-features --no-fail-fast`: 170 + others, all pass, 0 failures
- `cargo fmt --all --check`: pass
- `scripts/check-no-clippy-allow.sh`: pass

Re-running `code-review` on the resulting diff is the next step (iteration 2 of the loop).
