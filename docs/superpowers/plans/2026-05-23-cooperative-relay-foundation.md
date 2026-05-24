# Cooperative Relay — Foundation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Land the wire types, name encoding, default policy values, and filter-coverage check from the cooperative-relay design as a self-contained, dependency-free foundation in `crates/sunset-sync/src/routing/`. No engine integration, no behavior changes, no migration of existing call sites — those follow in subsequent plans.

**Architecture:** A new `routing` module under `sunset-sync` that hosts the data types (`SubscriptionEntry`, `LinkState`, `Neighbor`, `ProviderTick`, `SubscriptionPolicy`), the name-encoding helper that derives entry keys deterministically from `(filter, provider)`, the new reserved-name constants, and the pure `covers(superset, subset)` function over the existing `Filter` enum. Everything in this plan is pure data + pure functions; nothing reads or writes the store.

**Tech Stack:** Rust workspace (Cargo + nextest under `nix develop`). Wire format: postcard (`bincode` is deprecated and must not be reintroduced). Hashing: `blake3` (already in `sunset-sync`'s deps). Hex encoding: `hex` crate (workspace dep, needs to be added to `sunset-sync`'s `Cargo.toml`). Strict workspace clippy policy: no `#[allow(clippy::...)]` allowed in source.

**Spec:** `docs/superpowers/specs/2026-05-23-cooperative-relay-design.md`.

**Scope boundary:** This plan covers only what the spec's *Wire types* and *Policy surface* sections describe, plus the `covers` predicate used by the spec's *Candidate ranking* section. The receiver loop, provider loop, liveness, link-state publishing/consumption, ranking function, recursive subscription, voice integration, and migration of existing subscribe calls are each their own follow-up plan.

---

## File Structure

**New files:**
- `crates/sunset-sync/src/routing/mod.rs` — module entry; re-exports the public API.
- `crates/sunset-sync/src/routing/types.rs` — `SubscriptionEntry`, `LinkState`, `Neighbor`, `ProviderTick`.
- `crates/sunset-sync/src/routing/naming.rs` — `subscription_name(filter, provider)` and the routing reserved-name constants.
- `crates/sunset-sync/src/routing/policy.rs` — `SubscriptionPolicy` with named-constructor defaults.
- `crates/sunset-sync/src/routing/coverage.rs` — `covers(superset, subset)` over `Filter`.

**Modified files:**
- `crates/sunset-sync/Cargo.toml` — add `hex.workspace = true`.
- `crates/sunset-sync/src/lib.rs` — `pub mod routing;` plus re-exports.
- `crates/sunset-sync/src/reserved.rs` — leave `SUBSCRIBE_NAME` alone (still used by existing path); routing-specific constants live next to their users in `routing/naming.rs`.

**Test placement:** unit tests inline with each module (the workspace convention). No new files under `crates/sunset-sync/tests/`.

---

## Task 1: Routing module scaffolding

**Files:**
- Create: `crates/sunset-sync/src/routing/mod.rs`
- Modify: `crates/sunset-sync/src/lib.rs`

- [ ] **Step 1: Add the module file**

Write `crates/sunset-sync/src/routing/mod.rs`:

```rust
//! Cooperative-relay routing layer.
//!
//! See `docs/superpowers/specs/2026-05-23-cooperative-relay-design.md`.
//!
//! This module is the substrate (wire types, naming, policy, pure
//! predicates). The receiver loop, provider loop, liveness, and
//! integration into the engine ship in follow-up plans.
```

- [ ] **Step 2: Wire the module into `lib.rs`**

Read `crates/sunset-sync/src/lib.rs` and add `pub mod routing;` in the existing module-declaration block (alphabetical neighborhood: after `pub mod reserved;` and before `pub mod signaler;`).

- [ ] **Step 3: Verify the crate still builds**

Run: `nix develop --command cargo build -p sunset-sync`
Expected: success, no warnings.

- [ ] **Step 4: Commit**

```bash
git add crates/sunset-sync/src/routing/mod.rs crates/sunset-sync/src/lib.rs
git commit -m "$(cat <<'EOF'
sunset-sync: routing module scaffolding

Empty module entry for the cooperative-relay routing layer. Wire
types and pure helpers follow in subsequent commits.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 2: Add `hex` to sunset-sync dependencies

**Files:**
- Modify: `crates/sunset-sync/Cargo.toml`

- [ ] **Step 1: Inspect current dependency block**

Run: `grep -n "blake3\|^\[dependencies\]\|^\[dev" crates/sunset-sync/Cargo.toml`

- [ ] **Step 2: Add `hex.workspace = true` to the `[dependencies]` section**

The workspace already exports `hex = "0.4"`. Place the new line alphabetically (after `futures.workspace` or wherever `h*` belongs in the existing block).

- [ ] **Step 3: Verify it resolves**

Run: `nix develop --command cargo build -p sunset-sync`
Expected: success.

- [ ] **Step 4: Commit**

```bash
git add crates/sunset-sync/Cargo.toml
git commit -m "$(cat <<'EOF'
sunset-sync: depend on hex (for routing name encoding)

Used by routing::naming to render filter-hash and provider-id into
entry-name bytes. Already a workspace dep; new line in this crate's
manifest.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 3: SubscriptionEntry type + round-trip test

**Files:**
- Create: `crates/sunset-sync/src/routing/types.rs`
- Modify: `crates/sunset-sync/src/routing/mod.rs`

The `SubscriptionEntry` enum is the value stored at `(receiver_pubkey, subscription_name(filter, provider))`. `Active` carries the filter and provider redundantly with the key so providers can read what to forward without parsing the name; `Withdrawn` is published at the same key, with `expires_at` ≥ the previous entry's, to signal "stop forwarding" while still propagating through the network.

- [ ] **Step 1: Write the failing test**

Create `crates/sunset-sync/src/routing/types.rs`:

```rust
//! Wire types for the cooperative-relay routing layer.

use bytes::Bytes;
use serde::{Deserialize, Serialize};
use sunset_store::Filter;

use crate::types::PeerId;

/// Subscription state asserted by a receiver, addressed to one provider.
///
/// Stored at `(receiver_pubkey, naming::subscription_name(filter, provider))`
/// with normal LWW/TTL semantics. `Withdrawn` is published at the same
/// key with `expires_at` ≥ the previous entry's so it propagates through
/// the network like any other update before being garbage-collected.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SubscriptionEntry {
    /// The receiver wants `filter` from `provider`.
    Active { filter: Filter, provider: PeerId },
    /// The receiver no longer wants any data at this key.
    Withdrawn,
}

#[cfg(test)]
mod tests {
    use super::*;
    use sunset_store::VerifyingKey;

    fn vk(seed: &[u8]) -> VerifyingKey {
        VerifyingKey::new(Bytes::copy_from_slice(seed))
    }

    #[test]
    fn subscription_entry_active_postcard_roundtrip() {
        let entry = SubscriptionEntry::Active {
            filter: Filter::NamePrefix(Bytes::from_static(b"room/")),
            provider: PeerId(vk(b"provider-key")),
        };
        let bytes = postcard::to_stdvec(&entry).unwrap();
        let back: SubscriptionEntry = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(entry, back);
    }

    #[test]
    fn subscription_entry_withdrawn_postcard_roundtrip() {
        let entry = SubscriptionEntry::Withdrawn;
        let bytes = postcard::to_stdvec(&entry).unwrap();
        let back: SubscriptionEntry = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(entry, back);
    }
}
```

Add to `crates/sunset-sync/src/routing/mod.rs`:

```rust
pub mod types;

pub use types::SubscriptionEntry;
```

- [ ] **Step 2: Run the tests to confirm they fail (or pass)**

Run: `nix develop --command cargo test -p sunset-sync routing::types --no-fail-fast`
Expected: both tests pass on the first run because there's no implementation logic to fail — the test only exercises postcard derives. If they fail it's a derive-error or import mistake; fix the imports.

- [ ] **Step 3: Pin the Active variant's wire format with a hex vector**

The spec calls out wire-format drift protection (see the existing pinned vector for `ContentBlock::hash()` in `sunset-store/src/types.rs`). Append to the `tests` module:

```rust
#[test]
fn subscription_entry_active_wire_format_pinned_v1() {
    // Pinning postcard wire format. If this test breaks, you have either:
    //   - changed the SubscriptionEntry / Filter / PeerId encoding, or
    //   - bumped a postcard semver across an incompatible change.
    // Either is a wire-format break that needs a coordinated rollout.
    let entry = SubscriptionEntry::Active {
        filter: Filter::Namespace(Bytes::from_static(b"room/general")),
        provider: PeerId(VerifyingKey::new(Bytes::from_static(b"P"))),
    };
    let bytes = postcard::to_stdvec(&entry).unwrap();
    assert_eq!(hex::encode(&bytes), EXPECTED_HEX);
}

// Computed from the input above; regenerate intentionally on a real wire
// change with `cargo test ... -- --nocapture` then update.
const EXPECTED_HEX: &str = "PLACEHOLDER_HEX";
```

- [ ] **Step 4: Generate the actual hex by running the test once**

Run: `nix develop --command cargo test -p sunset-sync routing::types::subscription_entry_active_wire_format_pinned_v1 -- --nocapture 2>&1 | head -30`

Expected: the test fails with the actual hex in the assertion message (e.g., `left: "02020c726f6f6d2f67656e6572616c0101..."`). Copy that hex string verbatim and replace `PLACEHOLDER_HEX` with it.

- [ ] **Step 5: Re-run to confirm the pinned vector matches**

Run: `nix develop --command cargo test -p sunset-sync routing::types`
Expected: all three tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/sunset-sync/src/routing/types.rs crates/sunset-sync/src/routing/mod.rs
git commit -m "$(cat <<'EOF'
sunset-sync/routing: SubscriptionEntry wire type

Two-variant enum carried as the value at the per-(filter, provider)
subscription key. Round-trip plus hex-pinned v1 vector to catch wire
drift, matching the pattern used by ContentBlock::hash().

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 4: LinkState + Neighbor types

**Files:**
- Modify: `crates/sunset-sync/src/routing/types.rs`
- Modify: `crates/sunset-sync/src/routing/mod.rs`

`LinkState` is what each peer publishes about its own direct connections, at `(self_pubkey, naming::LINKS_NAME)`. Other peers read it to estimate RTT to indirect candidates and to grow their candidate set.

- [ ] **Step 1: Add the types and round-trip test**

Append to `crates/sunset-sync/src/routing/types.rs` (above the `#[cfg(test)]` block):

```rust
/// Self-published gossip of the publisher's direct neighbors.
///
/// Stored at `(self_pubkey, naming::LINKS_NAME)`. Receivers read this
/// from any peer they care about as input to the candidate ranking.
/// The publisher reports its own heartbeat measurements; no other field
/// (broad-subscriber flag, load hint) is carried because both are
/// derivable from data already replicated.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkState {
    pub neighbors: Vec<Neighbor>,
}

/// One row of `LinkState`: a peer the publisher is directly connected to,
/// with the publisher's most recent heartbeat-measured RTT and the
/// timestamp of the last successful exchange.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Neighbor {
    pub peer: PeerId,
    pub rtt_ms: u16,
    pub last_success_ts: u64,
}
```

Append to the `tests` module:

```rust
#[test]
fn link_state_postcard_roundtrip() {
    let ls = LinkState {
        neighbors: vec![
            Neighbor {
                peer: PeerId(vk(b"n1")),
                rtt_ms: 12,
                last_success_ts: 1_700_000_000,
            },
            Neighbor {
                peer: PeerId(vk(b"n2")),
                rtt_ms: 280,
                last_success_ts: 1_700_000_005,
            },
        ],
    };
    let bytes = postcard::to_stdvec(&ls).unwrap();
    let back: LinkState = postcard::from_bytes(&bytes).unwrap();
    assert_eq!(ls, back);
}

#[test]
fn link_state_empty_roundtrip() {
    let ls = LinkState { neighbors: vec![] };
    let bytes = postcard::to_stdvec(&ls).unwrap();
    let back: LinkState = postcard::from_bytes(&bytes).unwrap();
    assert_eq!(ls, back);
}
```

Update `crates/sunset-sync/src/routing/mod.rs` re-exports:

```rust
pub use types::{LinkState, Neighbor, SubscriptionEntry};
```

- [ ] **Step 2: Run tests**

Run: `nix develop --command cargo test -p sunset-sync routing::types`
Expected: 5 tests pass.

- [ ] **Step 3: Commit**

```bash
git add crates/sunset-sync/src/routing/types.rs crates/sunset-sync/src/routing/mod.rs
git commit -m "$(cat <<'EOF'
sunset-sync/routing: LinkState + Neighbor wire types

Publisher's self-reported direct connections. Receivers read this for
candidate discovery and RTT estimation. No broad-subscriber/load
fields; both are derivable from data already replicated.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 5: ProviderTick type

**Files:**
- Modify: `crates/sunset-sync/src/routing/types.rs`
- Modify: `crates/sunset-sync/src/routing/mod.rs`

`ProviderTick` is a small monotonic counter published periodically by every peer that may act as a provider. Receivers subscribed to ticks via a provider use their arrival cadence as the liveness signal for that provider's path.

- [ ] **Step 1: Add the type and test**

Append to `crates/sunset-sync/src/routing/types.rs` (above the `#[cfg(test)]` block):

```rust
/// Monotonic liveness beacon published by a provider.
///
/// Stored at `(self_pubkey, naming::PROVIDER_TICK_NAME)`. Receivers
/// observe arrival cadence on their subscribed path as the provider's
/// liveness signal; for active data streams (e.g. voice frames) the
/// data itself serves the same role.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderTick {
    pub seq: u64,
}
```

Append to the `tests` module:

```rust
#[test]
fn provider_tick_postcard_roundtrip() {
    for seq in [0u64, 1, 42, u64::MAX] {
        let t = ProviderTick { seq };
        let bytes = postcard::to_stdvec(&t).unwrap();
        let back: ProviderTick = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(t, back);
    }
}
```

Update `crates/sunset-sync/src/routing/mod.rs`:

```rust
pub use types::{LinkState, Neighbor, ProviderTick, SubscriptionEntry};
```

- [ ] **Step 2: Run tests**

Run: `nix develop --command cargo test -p sunset-sync routing::types`
Expected: 6 tests pass.

- [ ] **Step 3: Commit**

```bash
git add crates/sunset-sync/src/routing/types.rs crates/sunset-sync/src/routing/mod.rs
git commit -m "$(cat <<'EOF'
sunset-sync/routing: ProviderTick wire type

Monotonic liveness beacon. Receivers measure arrival cadence on the
subscribed path to detect dead providers; voice frames serve the
same role for active streams.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 6: Subscription name encoding + reserved-name constants

**Files:**
- Create: `crates/sunset-sync/src/routing/naming.rs`
- Modify: `crates/sunset-sync/src/routing/mod.rs`

The name `_sunset-sync/subscribe/<filter-hash>/<provider-id>` is derived deterministically so re-publishing the same `(filter, provider)` pair updates the same key (LWW-friendly idempotent refresh) and distinct pairs coexist as distinct entries. The two routing reserved-name constants (`LINKS_NAME`, `PROVIDER_TICK_NAME`) live here next to the function that uses the same convention.

- [ ] **Step 1: Write the failing test and the function it expects**

Create `crates/sunset-sync/src/routing/naming.rs`:

```rust
//! Routing-layer reserved-name constants and the deterministic encoder
//! for per-(filter, provider) subscription entry names.

use bytes::Bytes;
use sunset_store::Filter;

use crate::types::PeerId;

/// Reserved name for self-published link-state advertisements
/// (one entry per peer at `(self_pubkey, LINKS_NAME)`).
pub const LINKS_NAME: &[u8] = b"_sunset-sync/links";

/// Reserved name for the monotonic provider-tick liveness beacon
/// (one entry per peer at `(self_pubkey, PROVIDER_TICK_NAME)`).
pub const PROVIDER_TICK_NAME: &[u8] = b"_sunset-sync/provider-tick";

/// Common prefix of every per-(filter, provider) subscription entry name.
/// Useful as a filter prefix when subscribing to the control plane.
pub const SUBSCRIBE_PREFIX: &[u8] = b"_sunset-sync/subscribe/";

/// Build the entry name for a `(filter, provider)` subscription.
///
/// Format: `_sunset-sync/subscribe/<blake3(postcard(filter))_hex>/<provider_pubkey_hex>`.
///
/// Re-publishing the same `(filter, provider)` always lands at the same
/// key, so LWW just refreshes the TTL. Distinct pairs land at distinct
/// keys, so multiple providers per filter (e.g. during failover) coexist.
pub fn subscription_name(filter: &Filter, provider: &PeerId) -> Bytes {
    let filter_bytes = postcard::to_stdvec(filter).expect("postcard filter encode is infallible");
    let filter_hash = blake3::hash(&filter_bytes);
    let filter_hex = hex::encode(filter_hash.as_bytes());
    let provider_hex = hex::encode(provider.0.as_bytes());
    Bytes::from(format!(
        "_sunset-sync/subscribe/{filter_hex}/{provider_hex}"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sunset_store::VerifyingKey;

    fn vk(seed: &[u8]) -> VerifyingKey {
        VerifyingKey::new(Bytes::copy_from_slice(seed))
    }

    fn pid(seed: &[u8]) -> PeerId {
        PeerId(vk(seed))
    }

    #[test]
    fn same_pair_produces_same_name() {
        let f = Filter::Namespace(Bytes::from_static(b"room/general"));
        let p = pid(b"provider-1");
        assert_eq!(subscription_name(&f, &p), subscription_name(&f, &p));
    }

    #[test]
    fn different_filters_produce_different_names() {
        let p = pid(b"provider-1");
        let f1 = Filter::Namespace(Bytes::from_static(b"room/general"));
        let f2 = Filter::Namespace(Bytes::from_static(b"room/other"));
        assert_ne!(subscription_name(&f1, &p), subscription_name(&f2, &p));
    }

    #[test]
    fn different_providers_produce_different_names() {
        let f = Filter::Namespace(Bytes::from_static(b"room/general"));
        let p1 = pid(b"provider-1");
        let p2 = pid(b"provider-2");
        assert_ne!(subscription_name(&f, &p1), subscription_name(&f, &p2));
    }

    #[test]
    fn name_has_expected_prefix_and_shape() {
        let f = Filter::Specific(vk(b"writer"), Bytes::from_static(b"k"));
        let p = pid(b"provider-1");
        let name = subscription_name(&f, &p);
        let s = std::str::from_utf8(&name).expect("name is utf-8");
        assert!(s.starts_with("_sunset-sync/subscribe/"));
        // /<filter-hash-hex 64 chars>/<provider-hex>
        let rest = &s["_sunset-sync/subscribe/".len()..];
        let mut parts = rest.split('/');
        let filter_hex = parts.next().unwrap();
        let provider_hex = parts.next().unwrap();
        assert!(parts.next().is_none());
        assert_eq!(filter_hex.len(), 64); // blake3 = 32 bytes = 64 hex chars
        assert_eq!(provider_hex, hex::encode(b"provider-1"));
    }

    #[test]
    fn reserved_constants_are_under_sunset_sync_prefix() {
        assert!(LINKS_NAME.starts_with(b"_sunset-sync/"));
        assert!(PROVIDER_TICK_NAME.starts_with(b"_sunset-sync/"));
        assert!(SUBSCRIBE_PREFIX.starts_with(b"_sunset-sync/"));
    }
}
```

Update `crates/sunset-sync/src/routing/mod.rs`:

```rust
pub mod naming;
pub mod types;

pub use naming::{LINKS_NAME, PROVIDER_TICK_NAME, SUBSCRIBE_PREFIX, subscription_name};
pub use types::{LinkState, Neighbor, ProviderTick, SubscriptionEntry};
```

- [ ] **Step 2: Run tests**

Run: `nix develop --command cargo test -p sunset-sync routing::naming`
Expected: 5 tests pass.

- [ ] **Step 3: Commit**

```bash
git add crates/sunset-sync/src/routing/naming.rs crates/sunset-sync/src/routing/mod.rs
git commit -m "$(cat <<'EOF'
sunset-sync/routing: deterministic per-(filter, provider) entry name

subscription_name() derives the entry name as
_sunset-sync/subscribe/<blake3(postcard(filter))_hex>/<provider_hex>
so refreshes hit the same key (idempotent LWW) while distinct pairs
coexist. Plus the two flat reserved names for link-state and
provider-tick gossip.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 7: SubscriptionPolicy + named defaults

**Files:**
- Create: `crates/sunset-sync/src/routing/policy.rs`
- Modify: `crates/sunset-sync/src/routing/mod.rs`

The receiver runs one `SubscriptionPolicy` per filter. `target_n = 1` is reactive single-provider with propagation-bounded failover; `target_n = 2` is dual-delivery with gap-free failover (the voice-during-a-call mode). `freshness_threshold` is the only hysteresis knob — exceed it and the provider is treated as dead.

- [ ] **Step 1: Write the type and tests**

Create `crates/sunset-sync/src/routing/policy.rs`:

```rust
//! Per-filter policy parameters for the receiver-side routing loop.
//!
//! There are exactly two knobs:
//!
//! - `target_n` — how many healthy providers to maintain (1 = reactive,
//!   2 = dual-delivery for gap-free failover).
//! - `freshness_threshold` — how long the receiver waits without hearing
//!   anything via a provider before declaring it dead.
//!
//! Adding any third knob (per-provider weights, dwell times, switch
//! thresholds) would re-introduce the enumerated-cases-as-algorithm
//! anti-pattern the cooperative-relay design explicitly avoids.

use std::time::Duration;

/// Per-filter routing policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SubscriptionPolicy {
    pub target_n: usize,
    pub freshness_threshold: Duration,
}

impl SubscriptionPolicy {
    /// Reactive single-provider policy with a 5-second freshness budget
    /// — the default for reliable store-data subscriptions.
    pub const fn store_data() -> Self {
        Self {
            target_n: 1,
            freshness_threshold: Duration::from_secs(5),
        }
    }

    /// Dual-delivery policy with a 200ms freshness budget — used by the
    /// voice subsystem while a call is active, where gaps are perceptible.
    pub const fn voice_active_call() -> Self {
        Self {
            target_n: 2,
            freshness_threshold: Duration::from_millis(200),
        }
    }
}

impl Default for SubscriptionPolicy {
    /// Defaults to `store_data()` — the safe, low-bandwidth choice.
    fn default() -> Self {
        Self::store_data()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_data_defaults() {
        let p = SubscriptionPolicy::store_data();
        assert_eq!(p.target_n, 1);
        assert_eq!(p.freshness_threshold, Duration::from_secs(5));
    }

    #[test]
    fn voice_active_call_uses_dual_delivery() {
        let p = SubscriptionPolicy::voice_active_call();
        assert_eq!(p.target_n, 2);
        assert_eq!(p.freshness_threshold, Duration::from_millis(200));
    }

    #[test]
    fn default_matches_store_data() {
        assert_eq!(SubscriptionPolicy::default(), SubscriptionPolicy::store_data());
    }
}
```

Update `crates/sunset-sync/src/routing/mod.rs`:

```rust
pub mod naming;
pub mod policy;
pub mod types;

pub use naming::{LINKS_NAME, PROVIDER_TICK_NAME, SUBSCRIBE_PREFIX, subscription_name};
pub use policy::SubscriptionPolicy;
pub use types::{LinkState, Neighbor, ProviderTick, SubscriptionEntry};
```

- [ ] **Step 2: Run tests**

Run: `nix develop --command cargo test -p sunset-sync routing::policy`
Expected: 3 tests pass.

- [ ] **Step 3: Commit**

```bash
git add crates/sunset-sync/src/routing/policy.rs crates/sunset-sync/src/routing/mod.rs
git commit -m "$(cat <<'EOF'
sunset-sync/routing: SubscriptionPolicy with two named defaults

Two knobs only (target_n, freshness_threshold). Named constructors
for the two regimes the design calls out: store_data (reactive,
single-provider, 5s) and voice_active_call (dual-delivery, 200ms).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 8: Filter coverage — Specific cases

**Files:**
- Create: `crates/sunset-sync/src/routing/coverage.rs`
- Modify: `crates/sunset-sync/src/routing/mod.rs`

`covers(superset, subset)` is the pure predicate the receiver ranking uses to ask "would I get filter `f` for free if I subscribed to this candidate via the filter they already cover?" It returns `true` iff *every* `(vk, name)` matching `subset` also matches `superset`. This task gets the file in place and handles the `Specific` superset cases; subsequent tasks add the other four superset variants.

- [ ] **Step 1: Write the failing test**

Create `crates/sunset-sync/src/routing/coverage.rs`:

```rust
//! Pure predicate: does one filter cover (is a superset of) another?
//!
//! Used by the receiver ranking to ask "if a candidate already
//! subscribes to filter S, will my filter F be satisfied for free?"
//! Answer: yes iff every `(vk, name)` matching F also matches S, i.e.
//! `covers(S, F) == true`.

use bytes::Bytes;
use sunset_store::{Filter, VerifyingKey};

/// True iff every `(vk, name)` matching `subset` also matches `superset`.
///
/// Equivalent to "subscribing to `superset` would deliver everything
/// `subset` asks for." The relation is reflexive, transitive, and
/// not symmetric.
pub fn covers(superset: &Filter, subset: &Filter) -> bool {
    match superset {
        Filter::Specific(super_vk, super_name) => {
            covers_specific(super_vk, super_name, subset)
        }
        // Other superset variants implemented in subsequent tasks.
        _ => unimplemented!("covers: superset variant not yet implemented"),
    }
}

/// `Specific(super_vk, super_name)` covers `subset` iff `subset` matches
/// exactly that one key — i.e. `subset` is itself `Specific(super_vk,
/// super_name)`, or a `Union` whose every alternative is covered by it.
fn covers_specific(super_vk: &VerifyingKey, super_name: &Bytes, subset: &Filter) -> bool {
    match subset {
        Filter::Specific(sub_vk, sub_name) => super_vk == sub_vk && super_name == sub_name,
        Filter::Union(alts) => alts.iter().all(|alt| {
            covers(&Filter::Specific(super_vk.clone(), super_name.clone()), alt)
        }),
        Filter::Keyspace(_) | Filter::Namespace(_) | Filter::NamePrefix(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vk(seed: &[u8]) -> VerifyingKey {
        VerifyingKey::new(Bytes::copy_from_slice(seed))
    }

    fn n(b: &'static [u8]) -> Bytes {
        Bytes::from_static(b)
    }

    #[test]
    fn specific_covers_itself() {
        let f = Filter::Specific(vk(b"a"), n(b"k"));
        assert!(covers(&f, &f));
    }

    #[test]
    fn specific_does_not_cover_different_specific() {
        let s = Filter::Specific(vk(b"a"), n(b"k"));
        assert!(!covers(&s, &Filter::Specific(vk(b"b"), n(b"k"))));
        assert!(!covers(&s, &Filter::Specific(vk(b"a"), n(b"other"))));
    }

    #[test]
    fn specific_does_not_cover_broader_filters() {
        let s = Filter::Specific(vk(b"a"), n(b"k"));
        assert!(!covers(&s, &Filter::Keyspace(vk(b"a"))));
        assert!(!covers(&s, &Filter::Namespace(n(b"k"))));
        assert!(!covers(&s, &Filter::NamePrefix(n(b""))));
    }

    #[test]
    fn specific_covers_union_of_only_itself() {
        let s = Filter::Specific(vk(b"a"), n(b"k"));
        let single = Filter::Union(vec![Filter::Specific(vk(b"a"), n(b"k"))]);
        let two_same = Filter::Union(vec![
            Filter::Specific(vk(b"a"), n(b"k")),
            Filter::Specific(vk(b"a"), n(b"k")),
        ]);
        let mixed = Filter::Union(vec![
            Filter::Specific(vk(b"a"), n(b"k")),
            Filter::Specific(vk(b"b"), n(b"k")),
        ]);
        assert!(covers(&s, &single));
        assert!(covers(&s, &two_same));
        assert!(!covers(&s, &mixed));
    }
}
```

Update `crates/sunset-sync/src/routing/mod.rs`:

```rust
pub mod coverage;
pub mod naming;
pub mod policy;
pub mod types;

pub use coverage::covers;
pub use naming::{LINKS_NAME, PROVIDER_TICK_NAME, SUBSCRIBE_PREFIX, subscription_name};
pub use policy::SubscriptionPolicy;
pub use types::{LinkState, Neighbor, ProviderTick, SubscriptionEntry};
```

- [ ] **Step 2: Run tests**

Run: `nix develop --command cargo test -p sunset-sync routing::coverage`
Expected: 4 tests pass.

- [ ] **Step 3: Commit**

```bash
git add crates/sunset-sync/src/routing/coverage.rs crates/sunset-sync/src/routing/mod.rs
git commit -m "$(cat <<'EOF'
sunset-sync/routing: covers() — Specific superset cases

covers(superset, subset) is the pure predicate the receiver ranking
uses to know whether a candidate's existing subscription would
deliver our filter for free. This commit handles Specific supersets;
the other variants follow.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 9: Filter coverage — Keyspace superset

**Files:**
- Modify: `crates/sunset-sync/src/routing/coverage.rs`

`Keyspace(vk)` covers everything written by `vk` — every name under that writer. So it covers any `subset` whose matches are all under `vk`: another `Keyspace(vk)`, a `Specific(vk, _)`, a `Union` where every alternative is under `vk`. It does NOT cover `Namespace`, `NamePrefix`, or `Specific(other_vk, _)`.

- [ ] **Step 1: Extend `covers` and `covers_specific` is unchanged**

In `crates/sunset-sync/src/routing/coverage.rs`, replace the `_ => unimplemented!()` arm in `covers` and add a new helper:

```rust
pub fn covers(superset: &Filter, subset: &Filter) -> bool {
    match superset {
        Filter::Specific(super_vk, super_name) => {
            covers_specific(super_vk, super_name, subset)
        }
        Filter::Keyspace(super_vk) => covers_keyspace(super_vk, subset),
        // Other superset variants implemented in subsequent tasks.
        _ => unimplemented!("covers: superset variant not yet implemented"),
    }
}

/// `Keyspace(super_vk)` covers `subset` iff every match of `subset` is
/// written by `super_vk`.
fn covers_keyspace(super_vk: &VerifyingKey, subset: &Filter) -> bool {
    match subset {
        Filter::Specific(sub_vk, _) => super_vk == sub_vk,
        Filter::Keyspace(sub_vk) => super_vk == sub_vk,
        Filter::Union(alts) => alts.iter().all(|alt| {
            covers(&Filter::Keyspace(super_vk.clone()), alt)
        }),
        Filter::Namespace(_) | Filter::NamePrefix(_) => false,
    }
}
```

- [ ] **Step 2: Add tests for the Keyspace cases**

Append to the `tests` module:

```rust
#[test]
fn keyspace_covers_itself_and_specific_under_it() {
    let s = Filter::Keyspace(vk(b"a"));
    assert!(covers(&s, &s));
    assert!(covers(&s, &Filter::Specific(vk(b"a"), n(b"any"))));
    assert!(covers(&s, &Filter::Specific(vk(b"a"), n(b"other"))));
}

#[test]
fn keyspace_does_not_cover_other_writer() {
    let s = Filter::Keyspace(vk(b"a"));
    assert!(!covers(&s, &Filter::Keyspace(vk(b"b"))));
    assert!(!covers(&s, &Filter::Specific(vk(b"b"), n(b"k"))));
}

#[test]
fn keyspace_does_not_cover_writer_agnostic_filters() {
    let s = Filter::Keyspace(vk(b"a"));
    assert!(!covers(&s, &Filter::Namespace(n(b"k"))));
    assert!(!covers(&s, &Filter::NamePrefix(n(b""))));
}

#[test]
fn keyspace_covers_union_iff_all_alts_under_it() {
    let s = Filter::Keyspace(vk(b"a"));
    assert!(covers(&s, &Filter::Union(vec![
        Filter::Specific(vk(b"a"), n(b"k1")),
        Filter::Specific(vk(b"a"), n(b"k2")),
    ])));
    assert!(!covers(&s, &Filter::Union(vec![
        Filter::Specific(vk(b"a"), n(b"k1")),
        Filter::Specific(vk(b"b"), n(b"k1")),
    ])));
}
```

- [ ] **Step 3: Run tests**

Run: `nix develop --command cargo test -p sunset-sync routing::coverage`
Expected: 8 tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/sunset-sync/src/routing/coverage.rs
git commit -m "$(cat <<'EOF'
sunset-sync/routing: covers() — Keyspace superset

Keyspace(vk) covers any filter whose matches are all under vk:
itself, Specific(vk, *), or a Union where every alternative is.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 10: Filter coverage — Namespace superset

**Files:**
- Modify: `crates/sunset-sync/src/routing/coverage.rs`

`Namespace(name)` covers everything with that exact name across all writers. Covers `Specific(*, name)`, another `Namespace(name)`, and Unions where every alternative is. Does NOT cover `Keyspace`, `NamePrefix` (even of itself — a prefix can match other names too), or `Namespace(other_name)`.

- [ ] **Step 1: Extend `covers` and add the helper**

```rust
pub fn covers(superset: &Filter, subset: &Filter) -> bool {
    match superset {
        Filter::Specific(super_vk, super_name) => {
            covers_specific(super_vk, super_name, subset)
        }
        Filter::Keyspace(super_vk) => covers_keyspace(super_vk, subset),
        Filter::Namespace(super_name) => covers_namespace(super_name, subset),
        // Other superset variants implemented in subsequent tasks.
        _ => unimplemented!("covers: superset variant not yet implemented"),
    }
}

/// `Namespace(super_name)` covers `subset` iff every match of `subset` has
/// `name == super_name`.
fn covers_namespace(super_name: &Bytes, subset: &Filter) -> bool {
    match subset {
        Filter::Specific(_, sub_name) => super_name == sub_name,
        Filter::Namespace(sub_name) => super_name == sub_name,
        Filter::Union(alts) => alts.iter().all(|alt| {
            covers(&Filter::Namespace(super_name.clone()), alt)
        }),
        Filter::Keyspace(_) | Filter::NamePrefix(_) => false,
    }
}
```

- [ ] **Step 2: Add tests**

```rust
#[test]
fn namespace_covers_itself_and_specifics_with_same_name() {
    let s = Filter::Namespace(n(b"room/x"));
    assert!(covers(&s, &s));
    assert!(covers(&s, &Filter::Specific(vk(b"a"), n(b"room/x"))));
    assert!(covers(&s, &Filter::Specific(vk(b"b"), n(b"room/x"))));
}

#[test]
fn namespace_does_not_cover_other_name() {
    let s = Filter::Namespace(n(b"room/x"));
    assert!(!covers(&s, &Filter::Namespace(n(b"room/y"))));
    assert!(!covers(&s, &Filter::Specific(vk(b"a"), n(b"room/y"))));
}

#[test]
fn namespace_does_not_cover_writer_or_prefix_filters() {
    let s = Filter::Namespace(n(b"room/x"));
    assert!(!covers(&s, &Filter::Keyspace(vk(b"a"))));
    // Even NamePrefix matching only the same name string isn't covered —
    // a prefix matches anything-with-that-prefix, not the exact name.
    assert!(!covers(&s, &Filter::NamePrefix(n(b"room/x"))));
}
```

- [ ] **Step 3: Run tests**

Run: `nix develop --command cargo test -p sunset-sync routing::coverage`
Expected: 11 tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/sunset-sync/src/routing/coverage.rs
git commit -m "$(cat <<'EOF'
sunset-sync/routing: covers() — Namespace superset

Namespace(name) covers entries with that exact name across writers.
Does NOT cover NamePrefix even of itself — a prefix matches a wider
set of names than just the literal prefix string.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 11: Filter coverage — NamePrefix superset

**Files:**
- Modify: `crates/sunset-sync/src/routing/coverage.rs`

`NamePrefix(p)` covers any filter whose matches all have names starting with `p`. This covers:
- `Specific(_, name)` where `name.starts_with(p)`
- `Namespace(name)` where `name.starts_with(p)`
- `NamePrefix(longer)` where `longer.starts_with(p)`
- `Union(alts)` where every alt is covered

It does NOT cover `Keyspace` (writer-only, no name constraint).

- [ ] **Step 1: Extend `covers` and add the helper**

```rust
pub fn covers(superset: &Filter, subset: &Filter) -> bool {
    match superset {
        Filter::Specific(super_vk, super_name) => {
            covers_specific(super_vk, super_name, subset)
        }
        Filter::Keyspace(super_vk) => covers_keyspace(super_vk, subset),
        Filter::Namespace(super_name) => covers_namespace(super_name, subset),
        Filter::NamePrefix(super_prefix) => covers_name_prefix(super_prefix, subset),
        Filter::Union(super_alts) => covers_union(super_alts, subset),
    }
}

/// `NamePrefix(super_prefix)` covers `subset` iff every match of `subset`
/// has a name starting with `super_prefix`.
fn covers_name_prefix(super_prefix: &Bytes, subset: &Filter) -> bool {
    match subset {
        Filter::Specific(_, sub_name) => sub_name.starts_with(super_prefix.as_ref()),
        Filter::Namespace(sub_name) => sub_name.starts_with(super_prefix.as_ref()),
        Filter::NamePrefix(sub_prefix) => sub_prefix.starts_with(super_prefix.as_ref()),
        Filter::Union(alts) => alts.iter().all(|alt| {
            covers(&Filter::NamePrefix(super_prefix.clone()), alt)
        }),
        Filter::Keyspace(_) => false,
    }
}
```

Also stub `covers_union` for now (filled in next task):

```rust
fn covers_union(_super_alts: &[Filter], _subset: &Filter) -> bool {
    unimplemented!("covers: Union superset implemented in next task")
}
```

- [ ] **Step 2: Add tests**

```rust
#[test]
fn name_prefix_covers_specifics_under_prefix() {
    let s = Filter::NamePrefix(n(b"room/"));
    assert!(covers(&s, &Filter::Specific(vk(b"a"), n(b"room/general"))));
    assert!(covers(&s, &Filter::Specific(vk(b"a"), n(b"room/"))));
    assert!(!covers(&s, &Filter::Specific(vk(b"a"), n(b"other"))));
}

#[test]
fn name_prefix_covers_namespaces_and_longer_prefixes_under_it() {
    let s = Filter::NamePrefix(n(b"room/"));
    assert!(covers(&s, &Filter::Namespace(n(b"room/general"))));
    assert!(covers(&s, &Filter::NamePrefix(n(b"room/x/"))));
    assert!(covers(&s, &Filter::NamePrefix(n(b"room/"))));
    assert!(!covers(&s, &Filter::NamePrefix(n(b"r")))); // shorter, broader
}

#[test]
fn name_prefix_does_not_cover_keyspace() {
    let s = Filter::NamePrefix(n(b"room/"));
    assert!(!covers(&s, &Filter::Keyspace(vk(b"a"))));
}

#[test]
fn empty_prefix_covers_everything_name_based() {
    let s = Filter::NamePrefix(n(b""));
    assert!(covers(&s, &Filter::Specific(vk(b"x"), n(b"anything"))));
    assert!(covers(&s, &Filter::Namespace(n(b"anything"))));
    assert!(covers(&s, &Filter::NamePrefix(n(b"x/"))));
    // Still doesn't cover Keyspace (writer-keyed, not name-keyed).
    assert!(!covers(&s, &Filter::Keyspace(vk(b"x"))));
}
```

- [ ] **Step 3: Run tests**

Run: `nix develop --command cargo test -p sunset-sync routing::coverage`
Expected: 15 tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/sunset-sync/src/routing/coverage.rs
git commit -m "$(cat <<'EOF'
sunset-sync/routing: covers() — NamePrefix superset

NamePrefix(p) covers Specific/Namespace/longer-NamePrefix subsets
whose names start with p. Empty prefix covers all name-based filters
but not Keyspace.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 12: Filter coverage — Union superset and subset symmetry

**Files:**
- Modify: `crates/sunset-sync/src/routing/coverage.rs`

`Union(alts)` covers `subset` iff at least one alternative covers it. For `subset = Union(sub_alts)`, every alternative in `sub_alts` must be covered by at least one super alternative. The reverse-direction Union handling already exists in each `covers_*` helper above (they delegate `Union` subsets to `all(alts)` with the singleton superset).

- [ ] **Step 1: Replace the `covers_union` stub**

```rust
/// `Union(super_alts)` covers `subset` iff:
/// - `subset` is itself a `Union(sub_alts)` and every `sub_alt` is
///   covered by at least one `super_alt`, OR
/// - `subset` is a non-Union and some `super_alt` covers it.
fn covers_union(super_alts: &[Filter], subset: &Filter) -> bool {
    match subset {
        Filter::Union(sub_alts) => sub_alts
            .iter()
            .all(|sub_alt| super_alts.iter().any(|sup| covers(sup, sub_alt))),
        _ => super_alts.iter().any(|sup| covers(sup, subset)),
    }
}
```

- [ ] **Step 2: Add tests**

```rust
#[test]
fn union_superset_covers_when_any_alt_covers() {
    let s = Filter::Union(vec![
        Filter::Keyspace(vk(b"a")),
        Filter::NamePrefix(n(b"room/")),
    ]);
    assert!(covers(&s, &Filter::Specific(vk(b"a"), n(b"random"))));
    assert!(covers(&s, &Filter::Specific(vk(b"b"), n(b"room/x"))));
    assert!(!covers(&s, &Filter::Specific(vk(b"b"), n(b"other"))));
}

#[test]
fn union_superset_covers_union_subset_pairwise() {
    let s = Filter::Union(vec![
        Filter::Keyspace(vk(b"a")),
        Filter::NamePrefix(n(b"room/")),
    ]);
    let covered = Filter::Union(vec![
        Filter::Specific(vk(b"a"), n(b"k")),
        Filter::Namespace(n(b"room/x")),
    ]);
    let not_covered = Filter::Union(vec![
        Filter::Specific(vk(b"a"), n(b"k")),
        Filter::Specific(vk(b"b"), n(b"presence")),
    ]);
    assert!(covers(&s, &covered));
    assert!(!covers(&s, &not_covered));
}

#[test]
fn empty_union_covers_nothing_and_is_covered_by_anything() {
    // Empty Union as superset: no alternative can cover anything, so always false.
    let empty_super = Filter::Union(vec![]);
    assert!(!covers(&empty_super, &Filter::Specific(vk(b"a"), n(b"k"))));

    // Empty Union as subset: vacuous "every alt covered" → true.
    let real_super = Filter::Keyspace(vk(b"a"));
    let empty_sub = Filter::Union(vec![]);
    assert!(covers(&real_super, &empty_sub));
}

#[test]
fn covers_is_reflexive() {
    let filters = [
        Filter::Specific(vk(b"a"), n(b"k")),
        Filter::Keyspace(vk(b"a")),
        Filter::Namespace(n(b"k")),
        Filter::NamePrefix(n(b"k")),
        Filter::Union(vec![Filter::Keyspace(vk(b"a")), Filter::NamePrefix(n(b"r/"))]),
    ];
    for f in &filters {
        assert!(covers(f, f), "covers({f:?}, {f:?}) should be true");
    }
}
```

- [ ] **Step 3: Run tests**

Run: `nix develop --command cargo test -p sunset-sync routing::coverage`
Expected: 19 tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/sunset-sync/src/routing/coverage.rs
git commit -m "$(cat <<'EOF'
sunset-sync/routing: covers() — Union superset; reflexivity tested

Completes the covers() predicate over all five Filter variants.
Empty-Union edge cases handled and tested explicitly; reflexivity
verified across all variants.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 13: Final workspace verification

**Files:** none modified; this is the gate before PR.

- [ ] **Step 1: Run the full workspace tests**

Run: `nix develop --command cargo test --workspace --all-features --no-fail-fast`
Expected: all tests pass. If a test in another crate is unexpectedly red, investigate — this plan adds code only, it should not break anything elsewhere.

- [ ] **Step 2: Run clippy with the workspace policy**

Run: `nix develop --command cargo clippy --workspace --all-features --all-targets -- -D warnings`
Expected: no warnings. If clippy flags something in the new module, fix the root cause — `#[allow(clippy::...)]` is forbidden (see project CLAUDE.md).

- [ ] **Step 3: Run the format check**

Run: `nix develop --command cargo fmt --all --check`
Expected: clean.

- [ ] **Step 4: Run the no-clippy-suppressions guard**

Run: `nix develop --command bash scripts/check-no-clippy-allow.sh`
Expected: clean (exit 0).

- [ ] **Step 5: Confirm public API surface**

Run: `nix develop --command cargo doc -p sunset-sync --no-deps 2>&1 | tail -20`
Expected: builds clean. Open `target/doc/sunset_sync/routing/index.html` mentally and confirm the re-exports surface `covers`, `subscription_name`, `SubscriptionEntry`, `LinkState`, `Neighbor`, `ProviderTick`, `SubscriptionPolicy`, and the three reserved-name constants.

- [ ] **Step 6: Sanity-check the diff**

Run: `git log --oneline master..HEAD && echo --- && git diff --stat master..HEAD`
Expected: ~12 commits, all under `crates/sunset-sync/src/routing/` plus the one-line `Cargo.toml` and `lib.rs` edits and the design-doc + plan files. No edits to existing engine code, store code, or test infrastructure.

---

## Out of scope (follow-up plans)

These pieces of the cooperative-relay design are intentionally NOT in this plan and will each need their own writing-plans cycle:

- **Receiver loop** (`ProviderSet`, slot-filling, set invariant) — needs engine integration, depends on the types from Task 3–7.
- **Provider loop** (forwarding, recursive subscription on cache miss) — depends on engine event integration.
- **Liveness** (`provider-tick` publish/consume, freshness clock, implicit per-provider tick subscription) — needs engine timers + subscription state.
- **Link-state publishing/consumption** — needs engine integration with the heartbeat subsystem.
- **Candidate ranking** (`expected_first_data` function) — uses `covers` from this plan, plus link-state + tick observation infrastructure.
- **Migration of existing call sites** (replace `publish_subscription(Filter, ttl)` with the new `ProviderSet` API at every caller, drop `subscription_registry::SubscriptionRegistry`'s one-filter-per-peer assumption) — touches every existing subscriber.
- **Voice integration** (`SubscriptionPolicy::voice_active_call()` driven by call lifecycle) — needs the receiver loop and policy plumbing landed first.
- **User preference for dual-delivery toggle** — UI / config plumbing.

This foundation alone is shippable and reviewable: it adds dead-code types and pure functions, breaks nothing, and gives every subsequent plan a stable substrate to depend on.
