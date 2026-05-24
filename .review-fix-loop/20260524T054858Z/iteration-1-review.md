# Code Review — Iteration 1

## Scope

PR #104 — `sunset-store: extract shared SubscriptionList helper for backends`. Diff: `master..HEAD` on branch `cleanup/unify-subscription-list`. 7 files reviewed across 3 principles (21 cells, all completed).

## Expectation

Pure refactor: collapse two duplicate `Subscription`/`SubscriptionList` modules into one shared module in `sunset-store`. Backends consume the shared types. No behavior change, no test change required, no new tests required *unless* the unification reveals the shared module should be directly tested.

## Structural findings (from expectation pass)

The change is small, focused, and mechanical. No coherence concerns at the structural level. One borderline finding: the shared module's `SubscriptionList` doc comment carries load-bearing rationale for using `std::sync::Mutex` (allowed under the "why-comments earn their place" exception). Cargo.toml change (tokio: optional → hard dep) is necessary and correct, with a minor cleanup opportunity in `[dev-dependencies]`.

## Clusters

### Cluster: Broadcast call-site duplication preserved across backends

**Confidence:** high

**Root cause:** The PR unified `SubscriptionList` but left the **call-site pattern** duplicated. Both `MemoryStore::insert` and `FsStore::insert` manually orchestrate `broadcast(Inserted|Replaced)` followed by an optional `broadcast(BlobAdded)`. The ordering invariant ("entry events before blob events") lives in two places and is enforced by hand. The right shape: a higher-level helper on `SubscriptionList` (e.g., `publish_insert(entry_event, Option<blob_hash>)`) that encapsulates the ordering and is called from one line in each backend.

**Members:**
- `crates/sunset-store-memory/src/store.rs:121-130` — manual ordering of `Inserted`/`Replaced` then conditional `BlobAdded`
- `crates/sunset-store-fs/src/store.rs:114-127` — identical manual ordering

**Recommended order:** Single fix — extend `SubscriptionList` with a helper that takes the entry event and the optional blob hash and broadcasts in the correct order. Update both call sites.

### Cluster: `broadcast` enumerates Event variants at the wrong layer

**Confidence:** medium

**Root cause:** `SubscriptionList::broadcast` peers into `Event` to extract `(vk, name)` for filter matching. That extraction belongs on `Filter` or `Event`, not in subscription bookkeeping. Adding a new Event variant tomorrow requires editing `broadcast`. The right shape: `Filter::matches_event(&self, event: &Event) -> bool` (or `Event::keyed_match_target(&self) -> Option<(&VerifyingKey, &Bytes)>`), and `broadcast` becomes a single retain loop.

**Members:**
- `crates/sunset-store/src/subscription.rs:56-77` — `match event` inside `broadcast`

**Recommended order:** Single fix — add `Filter::matches_event` (or equivalent on `Event`) in `crates/sunset-store/src/filter.rs`; simplify `broadcast` to use it.

## Standalone findings

- **`crates/sunset-store/Cargo.toml:22`** — `tokio.workspace = true` in `[dev-dependencies]` is redundant now that tokio is in `[dependencies]`. (P1: same fact stored in two places.) Confidence: high. Trivial.
- **`crates/sunset-store/src/subscription.rs:28-39`** — Load-bearing struct-level doc comment justifying `.unwrap()` on `Mutex::lock()`. Borderline; allowed under the why-comment exception, but fragile. Confidence: low that this is a violation.
- **`crates/sunset-store/src/subscription.rs`** — No direct unit tests for `SubscriptionList`. The shared abstraction's claims are exercised only via backend integration tests. (P3: receipt for the claim lives at the wrong layer.) Confidence: medium.

## Suggested address order across clusters

1. **Broadcast call-site duplication preserved across backends** (high confidence) — biggest structural win; eliminates the duplication that motivated the PR in the first place.
2. **`broadcast` enumerates Event variants at the wrong layer** (medium confidence) — localized, follow-on improvement.

## Coverage matrix

| File | P1 structure | P2 layers | P3 tests |
|---|---|---|---|
| `crates/sunset-store/src/subscription.rs` | reviewed | reviewed | reviewed |
| `crates/sunset-store/src/lib.rs` | reviewed | reviewed | reviewed |
| `crates/sunset-store/Cargo.toml` | reviewed | reviewed | reviewed |
| `crates/sunset-store-memory/src/lib.rs` | reviewed | reviewed | reviewed |
| `crates/sunset-store-memory/src/store.rs` | reviewed | reviewed | reviewed |
| `crates/sunset-store-fs/src/lib.rs` | reviewed | reviewed | reviewed |
| `crates/sunset-store-fs/src/store.rs` | reviewed | reviewed | reviewed |

All 21 cells (7 files × 3 principles) attempted and completed. No cells failed.
