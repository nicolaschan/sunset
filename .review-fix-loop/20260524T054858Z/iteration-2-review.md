# Code Review — Iteration 2

## Scope
PR #104, cumulative diff `master..HEAD`. New file inspected for the second pass: only `crates/sunset-store/src/subscription.rs` (publish_insert added), `crates/sunset-store-memory/src/store.rs` and `crates/sunset-store-fs/src/store.rs` (call-site consumers).

## Re-check of iteration 1 findings

- **Cluster 1 (broadcast call-site duplication across backends)** — RESOLVED. Both backends now construct the entry event locally and call `subscriptions.publish_insert(&entry_event, blob_added)`. The ordering invariant lives in one place. Net code reduction: ~10 lines.
- **Cluster 2 (`broadcast` enumerates Event variants at wrong layer)** — UNCHANGED. The previous iteration deliberately scoped to Cluster 1. `subscription.rs:54-78` still inspects `Event` to extract `(vk, name)` for filter matching. Now the **top remaining cluster**.
- **Standalone (Cargo.toml dev-deps tokio redundant)** — UNCHANGED.
- **Standalone (no direct unit tests for SubscriptionList)** — UNCHANGED. Notably, `publish_insert` is now also covered only via integration tests.
- **Standalone (doc comment fragility on Mutex unwrap)** — UNCHANGED, low-confidence, borderline.

## Clusters

### Cluster: `broadcast` enumerates Event variants at the wrong layer

**Confidence:** medium

**Root cause:** `SubscriptionList::broadcast` peers into `Event` to extract `(vk, name)` for filter matching, then re-inspects the `Option` pair inside the retain loop. The Event-variant set is a property of `Event`/`Filter`, not of subscription bookkeeping. Adding a new keyed Event variant tomorrow requires editing `broadcast` to recognize it (silent miss-the-extraction risk). The right shape: `Filter::matches_event(&self, event: &Event) -> bool`, encapsulating variant-aware matching in `Filter` where matching logic lives. `broadcast` then becomes "for each weak sub, upgrade, if filter matches event then send" — single retain loop, no enumeration.

**Members:**
- `crates/sunset-store/src/subscription.rs:54-78` — `match event` block inside `broadcast` that extracts `(vk, name)`, plus a second `match (vk, name)` inside the retain loop to dispatch.

**Address order:** Single fix — add `Filter::matches_event` in `crates/sunset-store/src/filter.rs`; simplify `broadcast`.

## Standalone findings

- **`crates/sunset-store/Cargo.toml:22`** — `tokio.workspace = true` in `[dev-dependencies]` is redundant with the entry in `[dependencies]`. Same fact stored twice in the manifest. Confidence: high. Trivial.
- **`crates/sunset-store/src/subscription.rs`** — No direct unit tests for `SubscriptionList::{add, broadcast, publish_insert}`. Exercised only via per-backend integration tests; the shared abstraction's claims (filter match, BlobAdded/BlobRemoved fan-out, dropped-subscription reaping, publish_insert ordering) lack direct receipts. Confidence: medium.
- **`crates/sunset-store/src/subscription.rs:32-40`** — Doc comment essay on Mutex unwrap panic-freedom. Allowed under the why-comment exception, but fragile. Confidence: low.

## Suggested address order across clusters

1. **`broadcast` enumerates Event variants at the wrong layer** — sole cluster.
