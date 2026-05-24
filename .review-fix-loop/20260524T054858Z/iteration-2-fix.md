# Code Fix — Iteration 2

## Cluster addressed
**`broadcast` enumerates Event variants at the wrong layer**
Why this cluster: sole cluster in iteration 2's review; medium-confidence; localized.

## Root cause identified
`SubscriptionList::broadcast` previously inspected `Event` to extract `(vk, name)` for filter matching (using a `match event` block to populate an `Option` pair), then re-inspected that `Option` pair inside the retain loop. The Event-variant set is a property of `Event`/`Filter`, not of subscription bookkeeping; adding a new keyed `Event` variant tomorrow required editing `broadcast` to recognize it — a silent miss-the-extraction risk and a bolt-on the next time the enum grows.

## Structural shape chosen
Move the variant inspection onto `Filter` itself: add `Filter::matches_event(&self, event: &Event) -> bool` that knows which variants are keyed and which are fan-out-to-all. `broadcast` becomes a single retain loop that just asks the filter "are you interested in this event?" — no enumeration. Future `Event` variants are handled by extending `matches_event` in one place (next to `matches`), not by editing subscription bookkeeping.

## What was changed
- `crates/sunset-store/src/filter.rs` — added `Filter::matches_event(&self, event: &Event) -> bool` next to the existing `matches`.
- `crates/sunset-store/src/subscription.rs` — `broadcast` reduced to a single retain + `filter.matches_event(event)` check. The duplicated `(vk, name)` extraction is gone.

## What was deliberately not addressed
- Standalone: redundant `tokio.workspace = true` in `[dev-dependencies]`. (Trivial dedup; not part of this cluster.)
- Standalone: no direct unit tests for `SubscriptionList`. (Separate cluster shape; would need a test-writing iteration.)
- Standalone: doc comment fragility on `entries` Mutex unwrap. (Low confidence; not actionable.)

## Concerns surfaced during the fix
None. The change was strictly mechanical and the per-cell tests in `filter.rs` (filter matching) plus the per-backend conformance tests (subscribe + broadcast behavior) all pass unmodified.

## Verification
- `cargo clippy --workspace --all-features --all-targets -- -D warnings`: pass
- `cargo test --workspace --all-features --no-fail-fast`: all tests pass, 0 failures
