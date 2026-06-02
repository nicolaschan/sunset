# Cooperative Relay — Phase 2 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Land the two-layer data plane from `docs/superpowers/specs/2026-05-24-cooperative-relay-phase2-design.md`. After this plan: `subscribe`/`subscribe_via`/`unsubscribe`/`unsubscribe_via` are public; the legacy `publish_subscription`/`SubscriptionRegistry`/`SUBSCRIBE_NAME` wire path is deleted; all 5 production callsites are migrated; `forward_targets` is the one read path shared by reliable and ephemeral forwarding.

**Architecture:** `subscribe_via(filter, provider, policy)` is the mechanism — one signed `SubscriptionEntry` per call at `_sunset-sync/subscribe/<hash>/<provider>`. `subscribe(filter, policy)` is a thin auto-resubscriber that calls `subscribe_via` once per directly-connected peer and again on every future `AddPeer`. Inbound subscription state lives in `PeerSession::interests` so peer drop is one atomic removal. Outbound state lives in `Routes::my_subs`; the high-level intents that produced the outbound subs live in `Routes::broadcast_intents`. Forwarding consults `forward_targets(&peer_sessions, vk, name)` for both reliable and ephemeral paths; reliable additionally fires `DigestRequest` for backfill on subscribe arrival.

**Tech Stack:** Rust workspace under `nix develop`. Workspace clippy is `-D warnings`; `#[allow(clippy::...)]` is forbidden in source (enforced by `scripts/check-no-clippy-allow.sh`). Postcard wire format. Existing crate dependencies already include `blake3` and `hex` (added in Phase 1).

**Spec:** `docs/superpowers/specs/2026-05-24-cooperative-relay-phase2-design.md`.

---

## File Structure

**New files (in this plan):**
- `crates/sunset-sync/src/routing/routes.rs` — `Routes`, `OutboundKey`, `Outbound`, `BroadcastIntent`, `FilterHash` (moved here from a possible types.rs home), unit tests.
- `crates/sunset-sync/src/routing/forward.rs` — `forward_targets` free function and unit tests.
- `crates/sunset-sync/tests/phase2_subscribe.rs` — integration scenarios.

**Modified files:**
- `crates/sunset-sync/src/engine.rs` — biggest changes. PeerOutbound → PeerSession; new EngineState field; 4 new commands + handlers + public methods; SUBSCRIBE_PREFIX branch in handle_local_store_event; routing tick; auto-resubscriber hook in AddPeer; bootstrap rename.
- `crates/sunset-sync/src/routing/mod.rs` — re-exports.
- `crates/sunset-sync/src/reserved.rs` — drop `SUBSCRIBE_NAME` (at the end of the plan).
- `crates/sunset-sync/src/lib.rs` — drop `pub mod subscription_registry` (at the end of the plan).
- Five production callsites: `crates/sunset-relay/src/relay.rs` (×2), `crates/sunset-core/src/bus.rs`, `crates/sunset-core/src/peer/mod.rs` (×2).
- Test callsites: see Task 17.

**Deleted files:**
- `crates/sunset-sync/src/subscription_registry.rs` (at the end of the plan).

**Test-helper note:** the crate's integration tests (`tests/*.rs`) `use sunset_sync::test_transport`, which is feature-gated. Routing-scoped test runs must use:
```
nix develop --command cargo test -p sunset-sync --features test-helpers --lib routing
```
Workspace runs (final verification) use `--all-features`.

**`git add` gotcha:** the nix shellHook snapshots the git tree at shell entry, so new untracked files are invisible to `cargo` until staged. Always `git add` new files *before* invoking cargo.

---

## Task 1: `Routes` data structure + `FilterHash` alias

**Files:**
- Create: `crates/sunset-sync/src/routing/routes.rs`
- Modify: `crates/sunset-sync/src/routing/mod.rs`

The `Routes` struct holds my outgoing subscription state and high-level broadcast intents. Inbound interests live in `PeerSession` (added in a later task); this file is `Routes`-only.

- [ ] **Step 1: Write the file**

Create `crates/sunset-sync/src/routing/routes.rs`:

```rust
//! In-engine routing state: outbound subscriptions and broadcast intents.
//!
//! See `docs/superpowers/specs/2026-05-24-cooperative-relay-phase2-design.md`.
//!
//! Inbound interests (what other peers want from me) live in
//! `engine::PeerSession::interests`, not here. Per-peer state belongs with
//! the rest of the peer-keyed connection state so peer drop is one removal.

use std::collections::HashMap;
use std::time::Duration;

use sunset_store::Filter;

use crate::routing::policy::SubscriptionPolicy;
use crate::types::PeerId;

/// 32-byte blake3 hash of postcard(filter). Used as a key wherever the
/// filter itself would be redundant, or would force a `Hash` impl on
/// `Filter` (which the store doesn't currently provide).
pub type FilterHash = [u8; 32];

/// Key for `Routes::my_subs` — one entry per (filter, provider) pair.
#[derive(Clone, Debug, Hash, Eq, PartialEq)]
pub struct OutboundKey {
    pub filter_hash: FilterHash,
    pub provider: PeerId,
}

/// Value for `Routes::my_subs`.
#[derive(Clone, Debug)]
pub struct Outbound {
    pub filter: Filter,
    pub policy: SubscriptionPolicy,
    pub last_published_ms: u64,
}

/// Value for `Routes::broadcast_intents`. A user-issued `subscribe(filter)`
/// records one of these; the auto-resubscriber transforms each intent into
/// `Outbound` entries by calling `subscribe_via` for every connected peer.
#[derive(Clone, Debug)]
pub struct BroadcastIntent {
    pub filter: Filter,
    pub policy: SubscriptionPolicy,
}

pub struct Routes {
    me: PeerId,
    /// Outgoing subscription entries — one per (filter, provider) pair I've asked.
    pub my_subs: HashMap<OutboundKey, Outbound>,
    /// High-level intents from `subscribe(filter, policy)`. The auto-resubscriber
    /// reads this to decide what to subscribe-via on each peer connect, and to
    /// know what to tear down on `unsubscribe(filter)`.
    pub broadcast_intents: HashMap<FilterHash, BroadcastIntent>,
}

impl Routes {
    pub fn new(me: PeerId) -> Self {
        Self {
            me,
            my_subs: HashMap::new(),
            broadcast_intents: HashMap::new(),
        }
    }

    pub fn me(&self) -> &PeerId {
        &self.me
    }

    /// Returns the keys of `my_subs` whose `last_published_ms` is at least
    /// `policy.freshness_threshold / 2` behind `now_ms`.
    pub fn due_for_refresh(&self, now_ms: u64) -> Vec<OutboundKey> {
        self.my_subs
            .iter()
            .filter(|(_, ob)| {
                let half = ob.policy.freshness_threshold.as_millis() as u64 / 2;
                now_ms.saturating_sub(ob.last_published_ms) >= half
            })
            .map(|(k, _)| k.clone())
            .collect()
    }
}

/// Compute the `FilterHash` for a filter. Single source of truth used by
/// `routing::naming::subscription_name` and by callers that already have
/// the hash (e.g., decoded from an entry name).
pub fn filter_hash(filter: &Filter) -> FilterHash {
    let bytes = postcard::to_stdvec(filter).expect("postcard filter encode is infallible");
    *blake3::hash(&bytes).as_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use sunset_store::VerifyingKey;

    fn vk(seed: &[u8]) -> VerifyingKey {
        VerifyingKey::new(Bytes::copy_from_slice(seed))
    }

    fn pid(seed: &[u8]) -> PeerId {
        PeerId(vk(seed))
    }

    fn outbound(threshold_ms: u64, last_ms: u64) -> Outbound {
        Outbound {
            filter: Filter::NamePrefix(Bytes::from_static(b"x/")),
            policy: SubscriptionPolicy {
                target_n: 1,
                freshness_threshold: Duration::from_millis(threshold_ms),
            },
            last_published_ms: last_ms,
        }
    }

    #[test]
    fn due_for_refresh_returns_entries_past_half_threshold() {
        let mut routes = Routes::new(pid(b"me"));
        let key = OutboundKey { filter_hash: [0u8; 32], provider: pid(b"p") };
        routes.my_subs.insert(key.clone(), outbound(1000, 0));
        assert!(routes.due_for_refresh(499).is_empty()); // not yet half-threshold
        assert_eq!(routes.due_for_refresh(500), vec![key]); // at half-threshold
    }

    #[test]
    fn due_for_refresh_skips_fresh_entries() {
        let mut routes = Routes::new(pid(b"me"));
        let key = OutboundKey { filter_hash: [0u8; 32], provider: pid(b"p") };
        routes.my_subs.insert(key, outbound(1000, 800));
        assert!(routes.due_for_refresh(1000).is_empty()); // 200ms ago < 500ms threshold/2
    }

    #[test]
    fn filter_hash_is_deterministic() {
        let f = Filter::Namespace(Bytes::from_static(b"x"));
        assert_eq!(filter_hash(&f), filter_hash(&f));
    }

    #[test]
    fn filter_hash_differs_per_filter() {
        let a = Filter::Namespace(Bytes::from_static(b"x"));
        let b = Filter::Namespace(Bytes::from_static(b"y"));
        assert_ne!(filter_hash(&a), filter_hash(&b));
    }
}
```

Update `crates/sunset-sync/src/routing/mod.rs`:

```rust
//! Cooperative-relay routing layer.
//!
//! See `docs/superpowers/specs/2026-05-23-cooperative-relay-design.md`
//! and `docs/superpowers/specs/2026-05-24-cooperative-relay-phase2-design.md`.

pub mod coverage;
pub mod naming;
pub mod policy;
pub mod routes;
pub mod types;

pub use coverage::covers;
pub use naming::{LINKS_NAME, PROVIDER_TICK_NAME, SUBSCRIBE_PREFIX, subscription_name};
pub use policy::SubscriptionPolicy;
pub use routes::{BroadcastIntent, FilterHash, Outbound, OutboundKey, Routes, filter_hash};
pub use types::{LinkState, Neighbor, ProviderTick, SubscriptionEntry};
```

- [ ] **Step 2: Stage and run tests**

Run: `git add crates/sunset-sync/src/routing/routes.rs`
Run: `nix develop --command cargo test -p sunset-sync --features test-helpers --lib routing::routes`
Expected: 4 tests pass.

- [ ] **Step 3: Commit**

```bash
git add crates/sunset-sync/src/routing/routes.rs crates/sunset-sync/src/routing/mod.rs
git commit -m "$(cat <<'EOF'
sunset-sync/routing: Routes + OutboundKey/Outbound/BroadcastIntent

Outbound subscription state and high-level broadcast intents (the
auto-resubscriber's input). Inbound interests will live in
PeerSession::interests in a later task. due_for_refresh is the one
non-trivial query Routes carries; everything else is direct field
access.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 2: `forward_targets` free function

**Files:**
- Create: `crates/sunset-sync/src/routing/forward.rs`
- Modify: `crates/sunset-sync/src/routing/mod.rs`

`forward_targets` answers "given an event, which peers should I send it to?" Reads from per-peer interests (a future `PeerSession` field). Free function rather than `Routes` method because the data lives in `peer_sessions`, not `Routes`. Returns `HashSet` so dedup is in the type.

This task takes a generic over the per-peer container so it can be unit-tested without depending on the (not-yet-renamed) `PeerSession` struct. The engine adapter is a one-liner in a later task.

- [ ] **Step 1: Write the file**

Create `crates/sunset-sync/src/routing/forward.rs`:

```rust
//! `forward_targets`: given an event, which peers should I forward it to?
//!
//! Free function because the input data — per-peer interests — lives in
//! `engine::EngineState::peer_sessions`, not in `routing::Routes`.

use std::collections::{HashMap, HashSet};

use sunset_store::{Filter, VerifyingKey};

use crate::routing::FilterHash;
use crate::types::PeerId;

/// Generic over the per-peer container to keep this function testable in
/// isolation. The engine adapter passes `&peer_sessions` plus a closure
/// that pulls the `interests: HashMap<FilterHash, Filter>` out of each
/// `PeerSession`.
pub fn forward_targets<S, F>(
    peers: &HashMap<PeerId, S>,
    interests: F,
    vk: &VerifyingKey,
    name: &[u8],
) -> HashSet<PeerId>
where
    F: Fn(&S) -> &HashMap<FilterHash, Filter>,
{
    peers
        .iter()
        .filter(|(_, sess)| interests(sess).values().any(|f| f.matches(vk, name)))
        .map(|(p, _)| p.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use sunset_store::VerifyingKey;

    fn vk(seed: &[u8]) -> VerifyingKey {
        VerifyingKey::new(Bytes::copy_from_slice(seed))
    }

    fn pid(seed: &[u8]) -> PeerId {
        PeerId(vk(seed))
    }

    fn one(filter: Filter) -> HashMap<FilterHash, Filter> {
        let mut m = HashMap::new();
        m.insert(crate::routing::filter_hash(&filter), filter);
        m
    }

    #[test]
    fn returns_each_matching_peer_once() {
        let mut peers: HashMap<PeerId, HashMap<FilterHash, Filter>> = HashMap::new();
        peers.insert(pid(b"alice"), one(Filter::NamePrefix(Bytes::from_static(b"room/"))));
        peers.insert(pid(b"bob"), one(Filter::Keyspace(vk(b"writer"))));
        peers.insert(pid(b"carol"), HashMap::new());

        let targets = forward_targets(&peers, |s| s, &vk(b"writer"), b"room/x");
        assert_eq!(targets.len(), 2);
        assert!(targets.contains(&pid(b"alice"))); // matches by prefix
        assert!(targets.contains(&pid(b"bob")));   // matches by keyspace
    }

    #[test]
    fn empty_peers_returns_empty_set() {
        let peers: HashMap<PeerId, HashMap<FilterHash, Filter>> = HashMap::new();
        let targets = forward_targets(&peers, |s| s, &vk(b"x"), b"name");
        assert!(targets.is_empty());
    }

    #[test]
    fn multi_interest_peer_matches_on_any_one() {
        let mut interests = HashMap::new();
        let f1 = Filter::NamePrefix(Bytes::from_static(b"a/"));
        let f2 = Filter::NamePrefix(Bytes::from_static(b"b/"));
        interests.insert(crate::routing::filter_hash(&f1), f1);
        interests.insert(crate::routing::filter_hash(&f2), f2);
        let mut peers = HashMap::new();
        peers.insert(pid(b"p"), interests);

        let t1 = forward_targets(&peers, |s| s, &vk(b"x"), b"a/x");
        let t2 = forward_targets(&peers, |s| s, &vk(b"x"), b"b/y");
        let t3 = forward_targets(&peers, |s| s, &vk(b"x"), b"c/z");
        assert!(t1.contains(&pid(b"p")));
        assert!(t2.contains(&pid(b"p")));
        assert!(t3.is_empty());
    }
}
```

Update `crates/sunset-sync/src/routing/mod.rs` to add the new module and re-export:

```rust
pub mod forward;
// ... existing module declarations and re-exports ...
pub use forward::forward_targets;
```

- [ ] **Step 2: Stage and run tests**

Run: `git add crates/sunset-sync/src/routing/forward.rs`
Run: `nix develop --command cargo test -p sunset-sync --features test-helpers --lib routing::forward`
Expected: 3 tests pass.

- [ ] **Step 3: Commit**

```bash
git add crates/sunset-sync/src/routing/forward.rs crates/sunset-sync/src/routing/mod.rs
git commit -m "$(cat <<'EOF'
sunset-sync/routing: forward_targets free function

Free function (not a Routes method) because the per-peer interests
data will live in PeerSession (engine-side), not in routing::Routes.
Generic over the container so it's unit-testable without depending on
the not-yet-introduced PeerSession type.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 3: Rename `PeerOutbound` → `PeerSession` and add `interests`

**Files:**
- Modify: `crates/sunset-sync/src/engine.rs` (struct definition + ~30 field-access sites)

The struct gains one field. Renaming `PeerOutbound` → `PeerSession` makes its name match what it now is (a session, not just an outbound channel). `peer_outbound` field on `EngineState` becomes `peer_sessions`.

- [ ] **Step 1: Rename the struct and add the field**

In `crates/sunset-sync/src/engine.rs` (around line 159):

```rust
/// Per-peer connection state. Bundles outbound channel, transport identity,
/// the per-peer task shutdown handle, and the inbound interests (what this
/// peer currently wants from me).
pub(crate) struct PeerSession {
    pub(crate) conn_id: ConnectionId,
    pub(crate) kind: crate::transport::TransportKind,
    pub(crate) tx: mpsc::UnboundedSender<SyncMessage>,
    pub(crate) _shutdown: watch::Sender<()>,
    /// What this peer currently wants from me, keyed by `FilterHash` for
    /// O(1) Withdrawn lookups (the entry name carries the hash, not the
    /// filter). Populated from incoming `SubscriptionEntry::Active` events
    /// that name `me` as provider; cleared with the session on peer drop.
    pub(crate) interests: std::collections::HashMap<crate::routing::FilterHash, sunset_store::Filter>,
}
```

(The existing doc comment on the struct should be preserved or condensed; the load-bearing `_shutdown` documentation should stay.)

- [ ] **Step 2: Rename the field on EngineState**

In `EngineState`, change `pub peer_outbound: HashMap<PeerId, PeerOutbound>` to `pub peer_sessions: HashMap<PeerId, PeerSession>`.

- [ ] **Step 3: Mass-rename callsites**

Run `grep -n "peer_outbound\|PeerOutbound" crates/sunset-sync/src/engine.rs | wc -l` to count occurrences (expected ~30). Replace `peer_outbound` → `peer_sessions` and `PeerOutbound` → `PeerSession` throughout the file. The `Default::default()` initializer for `interests` should auto-fill (since `HashMap` is `Default`); if any `PeerSession { ... }` literal exists, add `interests: HashMap::new(),`.

- [ ] **Step 4: Verify it builds**

Run: `nix develop --command cargo build -p sunset-sync`
Expected: success, no warnings.

Run: `nix develop --command cargo test -p sunset-sync --features test-helpers --lib`
Expected: existing unit tests pass unchanged.

- [ ] **Step 5: Commit**

```bash
git add crates/sunset-sync/src/engine.rs
git commit -m "$(cat <<'EOF'
sunset-sync: rename PeerOutbound -> PeerSession; add interests field

The struct now holds outbound channel, transport kind, shutdown handle,
AND inbound interests for the peer. Bundling these per-peer fields
together makes peer drop a single atomic remove(), eliminating the
"remember to also clean up interests too" footgun the legacy
SubscriptionRegistry had.

interests is empty for every existing site that constructs PeerSession;
the SUBSCRIBE_PREFIX branch in handle_local_store_event (added in a
later task) will populate it.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 4: Add `routes` field to `EngineState`

**Files:**
- Modify: `crates/sunset-sync/src/engine.rs`

`Routes` is initialized empty in `EngineState::new`. Existing `registry` and `own_filters` stay temporarily — they'll be deleted in Task 13. Coexistence is fine; nothing crosses between them yet.

- [ ] **Step 1: Add the field and initializer**

In `EngineState` definition (around line 193), after `pub registry: SubscriptionRegistry`:

```rust
    pub registry: SubscriptionRegistry,
    pub routes: crate::routing::Routes,
```

In `EngineState::new` (around line 259), after `registry: SubscriptionRegistry::new()`:

```rust
    registry: SubscriptionRegistry::new(),
    routes: crate::routing::Routes::new(PeerId(local_signing.verifying_key())),
```

(The local PeerId is needed by `Routes::new`. Adjust the argument source if `EngineState::new` doesn't already have access to the verifying key — pass it in or pull from the signer parameter as the surrounding code does.)

- [ ] **Step 2: Build**

Run: `nix develop --command cargo build -p sunset-sync`
Expected: success.

- [ ] **Step 3: Commit**

```bash
git add crates/sunset-sync/src/engine.rs
git commit -m "$(cat <<'EOF'
sunset-sync: add Routes field to EngineState (coexists with registry)

routes starts empty; will be written by subscribe_via and the
SUBSCRIBE_PREFIX branch in subsequent tasks. SubscriptionRegistry
stays until callsites are migrated.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 5: `subscribe_via` API — command, handler, public method

**Files:**
- Modify: `crates/sunset-sync/src/engine.rs`

Adds the EngineCommand variant, the do_subscribe_via implementation, and the public method. After this task, callers can call `engine.subscribe_via(...)` and get an entry persisted, but the provider side (PeerSession::interests population, forwarding) isn't wired yet — that's Task 8.

- [ ] **Step 1: Add the EngineCommand variant**

In the `EngineCommand` enum (around line 117 where `PublishSubscription` is), add:

```rust
    SubscribeVia {
        filter: Filter,
        provider: PeerId,
        policy: crate::routing::SubscriptionPolicy,
        ack: oneshot::Sender<Result<()>>,
    },
    UnsubscribeVia {
        filter: Filter,
        provider: PeerId,
        ack: oneshot::Sender<Result<()>>,
    },
```

- [ ] **Step 2: Add the command-dispatch arms**

In the command-dispatch match (around line 559 where `PublishSubscription` is handled), add:

```rust
    EngineCommand::SubscribeVia { filter, provider, policy, ack } => {
        let r = self.do_subscribe_via(filter, provider, policy).await;
        let _ = ack.send(r);
    }
    EngineCommand::UnsubscribeVia { filter, provider, ack } => {
        let r = self.do_unsubscribe_via(filter, provider).await;
        let _ = ack.send(r);
    }
```

- [ ] **Step 3: Add the do_subscribe_via implementation**

Add to the `impl SyncEngine` block (place near `do_publish_subscription`, around line 1164):

```rust
    /// Publish a per-pair SubscriptionEntry::Active for (filter, provider)
    /// and record the outbound in `routes.my_subs` for refresh.
    async fn do_subscribe_via(
        &self,
        filter: Filter,
        provider: PeerId,
        policy: crate::routing::SubscriptionPolicy,
    ) -> Result<()> {
        use sunset_store::canonical::signing_payload;
        use sunset_store::{ContentBlock, SignedKvEntry};

        let filter_hash = crate::routing::filter_hash(&filter);
        let name = crate::routing::subscription_name(&filter, &provider);
        let entry_value = crate::routing::SubscriptionEntry::Active {
            filter: filter.clone(),
            provider: provider.clone(),
        };
        let value = postcard::to_stdvec(&entry_value)
            .map_err(|e| Error::Decode(format!("encode SubscriptionEntry: {e}")))?;
        let block = ContentBlock {
            data: Bytes::from(value),
            references: vec![],
        };
        let now_ms = web_time::SystemTime::now()
            .duration_since(web_time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let ttl_ms = policy.freshness_threshold.as_millis() as u64;
        let vk = self.signer.verifying_key();
        let value_hash = block.hash();
        let signature_payload = signing_payload(
            &vk, &name, &value_hash, now_ms, now_ms.saturating_add(ttl_ms),
        );
        let signature = self.signer.sign(&signature_payload);
        let entry = SignedKvEntry {
            verifying_key: vk,
            name,
            value_hash,
            priority: now_ms,
            expires_at: now_ms.saturating_add(ttl_ms),
            signature,
        };
        self.store.insert(entry, Some(block)).await?;

        let mut state = self.state.lock().await;
        state.routes.my_subs.insert(
            crate::routing::OutboundKey { filter_hash, provider: provider.clone() },
            crate::routing::Outbound { filter, policy, last_published_ms: now_ms },
        );
        Ok(())
    }

    async fn do_unsubscribe_via(
        &self,
        filter: Filter,
        provider: PeerId,
    ) -> Result<()> {
        use sunset_store::canonical::signing_payload;
        use sunset_store::{ContentBlock, SignedKvEntry};

        let filter_hash = crate::routing::filter_hash(&filter);
        let key = crate::routing::OutboundKey { filter_hash, provider: provider.clone() };
        let prev = {
            let mut state = self.state.lock().await;
            state.routes.my_subs.remove(&key)
        };
        let Some(prev) = prev else { return Ok(()) };
        let name = crate::routing::subscription_name(&filter, &provider);
        let entry_value = crate::routing::SubscriptionEntry::Withdrawn;
        let value = postcard::to_stdvec(&entry_value)
            .map_err(|e| Error::Decode(format!("encode SubscriptionEntry: {e}")))?;
        let block = ContentBlock {
            data: Bytes::from(value),
            references: vec![],
        };
        let now_ms = web_time::SystemTime::now()
            .duration_since(web_time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let ttl_ms = prev.policy.freshness_threshold.as_millis() as u64;
        let vk = self.signer.verifying_key();
        let value_hash = block.hash();
        let signature_payload = signing_payload(
            &vk, &name, &value_hash, now_ms, now_ms.saturating_add(ttl_ms),
        );
        let signature = self.signer.sign(&signature_payload);
        let entry = SignedKvEntry {
            verifying_key: vk,
            name,
            value_hash,
            priority: now_ms,
            expires_at: now_ms.saturating_add(ttl_ms),
            signature,
        };
        self.store.insert(entry, Some(block)).await?;
        Ok(())
    }
```

- [ ] **Step 4: Add the public methods**

Add to `impl SyncEngine` near `publish_subscription` (around line 390):

```rust
    /// Subscribe to `filter` from one specific peer. The provider, on
    /// receiving the resulting SubscriptionEntry, starts forwarding
    /// matching store events to me; the existing DigestRequest/Exchange
    /// pipeline backfills already-stored entries.
    pub async fn subscribe_via(
        &self,
        filter: Filter,
        provider: PeerId,
        policy: crate::routing::SubscriptionPolicy,
    ) -> Result<()> {
        let (ack, rx) = oneshot::channel();
        self.cmd_tx
            .send(EngineCommand::SubscribeVia { filter, provider, policy, ack })
            .map_err(|_| Error::Closed)?;
        rx.await.map_err(|_| Error::Closed)?
    }

    /// Withdraw a `subscribe_via(filter, provider)` subscription. Publishes
    /// `SubscriptionEntry::Withdrawn` at the same key; idempotent.
    pub async fn unsubscribe_via(
        &self,
        filter: Filter,
        provider: PeerId,
    ) -> Result<()> {
        let (ack, rx) = oneshot::channel();
        self.cmd_tx
            .send(EngineCommand::UnsubscribeVia { filter, provider, ack })
            .map_err(|_| Error::Closed)?;
        rx.await.map_err(|_| Error::Closed)?
    }
```

- [ ] **Step 5: Build and test**

Run: `nix develop --command cargo test -p sunset-sync --features test-helpers --lib`
Expected: existing tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/sunset-sync/src/engine.rs
git commit -m "$(cat <<'EOF'
sunset-sync: subscribe_via / unsubscribe_via public API

The mechanism layer: publish a per-pair SubscriptionEntry under
_sunset-sync/subscribe/<hash>/<provider>, record in routes.my_subs.
Provider-side handling (PeerSession::interests population, forwarding)
is wired up in subsequent tasks; this task only persists the entry.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 6: `subscribe` API + auto-resubscriber

**Files:**
- Modify: `crates/sunset-sync/src/engine.rs`

The high-level subscribe iterates connected peers and calls subscribe_via for each. On each future `AddPeer`, the engine re-runs the loop for any active broadcast intent.

- [ ] **Step 1: Add Subscribe/Unsubscribe EngineCommand variants**

```rust
    Subscribe {
        filter: Filter,
        policy: crate::routing::SubscriptionPolicy,
        ack: oneshot::Sender<Result<()>>,
    },
    Unsubscribe {
        filter: Filter,
        ack: oneshot::Sender<Result<()>>,
    },
```

- [ ] **Step 2: Add command-dispatch arms**

```rust
    EngineCommand::Subscribe { filter, policy, ack } => {
        let r = self.do_subscribe(filter, policy).await;
        let _ = ack.send(r);
    }
    EngineCommand::Unsubscribe { filter, ack } => {
        let r = self.do_unsubscribe(filter).await;
        let _ = ack.send(r);
    }
```

- [ ] **Step 3: Implement do_subscribe / do_unsubscribe**

```rust
    /// Declare interest in `filter` from any directly-connected peer.
    /// Records a BroadcastIntent and calls subscribe_via for every peer
    /// currently in peer_sessions. Future peer connects (handled in
    /// AddPeer) re-run subscribe_via for the new peer.
    async fn do_subscribe(
        &self,
        filter: Filter,
        policy: crate::routing::SubscriptionPolicy,
    ) -> Result<()> {
        let filter_hash = crate::routing::filter_hash(&filter);
        let peers: Vec<PeerId> = {
            let mut state = self.state.lock().await;
            state.routes.broadcast_intents.insert(
                filter_hash,
                crate::routing::BroadcastIntent { filter: filter.clone(), policy },
            );
            state.peer_sessions.keys().cloned().collect()
        };
        for peer in peers {
            self.do_subscribe_via(filter.clone(), peer, policy).await?;
        }
        Ok(())
    }

    async fn do_unsubscribe(&self, filter: Filter) -> Result<()> {
        let filter_hash = crate::routing::filter_hash(&filter);
        let providers: Vec<PeerId> = {
            let mut state = self.state.lock().await;
            if state.routes.broadcast_intents.remove(&filter_hash).is_none() {
                return Ok(());
            }
            state
                .routes
                .my_subs
                .keys()
                .filter(|k| k.filter_hash == filter_hash)
                .map(|k| k.provider.clone())
                .collect()
        };
        for provider in providers {
            self.do_unsubscribe_via(filter.clone(), provider).await?;
        }
        Ok(())
    }
```

- [ ] **Step 4: Add public methods**

```rust
    /// Declare interest in `filter` from any directly-connected peer.
    /// Implemented as an auto-resubscriber: for each currently-connected
    /// peer, calls subscribe_via; on future peer connects, re-runs.
    pub async fn subscribe(
        &self,
        filter: Filter,
        policy: crate::routing::SubscriptionPolicy,
    ) -> Result<()> {
        let (ack, rx) = oneshot::channel();
        self.cmd_tx
            .send(EngineCommand::Subscribe { filter, policy, ack })
            .map_err(|_| Error::Closed)?;
        rx.await.map_err(|_| Error::Closed)?
    }

    pub async fn unsubscribe(&self, filter: Filter) -> Result<()> {
        let (ack, rx) = oneshot::channel();
        self.cmd_tx
            .send(EngineCommand::Unsubscribe { filter, ack })
            .map_err(|_| Error::Closed)?;
        rx.await.map_err(|_| Error::Closed)?
    }
```

- [ ] **Step 5: Build and test**

Run: `nix develop --command cargo test -p sunset-sync --features test-helpers --lib`
Expected: pass.

- [ ] **Step 6: Commit**

```bash
git add crates/sunset-sync/src/engine.rs
git commit -m "$(cat <<'EOF'
sunset-sync: subscribe / unsubscribe public API + auto-resubscriber

The policy layer over subscribe_via. subscribe(F, policy) records a
BroadcastIntent and fan-outs to subscribe_via for every currently-
connected peer; unsubscribe(F) walks my_subs for the intent's
filter_hash and unsubscribe_via's each.

AddPeer hook to handle future peer connects lands in the next task.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 7: Auto-resubscriber hook on `AddPeer`

**Files:**
- Modify: `crates/sunset-sync/src/engine.rs`

When a new peer connects, iterate `broadcast_intents` and call `do_subscribe_via` for each. Place this immediately after the existing `peer_sessions.insert(...)` in the `AddPeer` handler (around line 600).

- [ ] **Step 1: Find the insert site**

Run: `grep -n "peer_sessions.insert\|EngineCommand::AddPeer" crates/sunset-sync/src/engine.rs | head -5`
Expected: an `AddPeer` arm and a `peer_sessions.insert(...)` call within it. Located around line 600.

- [ ] **Step 2: Add the hook**

Immediately after the insert (inside the same arm, while still holding the state lock or right after dropping it), collect the broadcast intents and call subscribe_via:

```rust
    // Inside EngineCommand::AddPeer handler, after peer_sessions.insert(...):
    let intents: Vec<crate::routing::BroadcastIntent> = {
        state.routes.broadcast_intents.values().cloned().collect()
    };
    drop(state); // release lock before re-acquiring in do_subscribe_via
    for intent in intents {
        // Errors here would leave my_subs out of sync with broadcast_intents;
        // log and continue rather than failing the AddPeer ack.
        if let Err(e) = self.do_subscribe_via(intent.filter, peer_id.clone(), intent.policy).await {
            tracing::warn!(?e, ?peer_id, "auto-resubscribe failed on new peer");
        }
    }
```

(Adjust the exact lock-drop placement to match the surrounding code's pattern.)

- [ ] **Step 3: Build and test**

Run: `nix develop --command cargo test -p sunset-sync --features test-helpers --lib`
Expected: pass. Existing two_peer_sync.rs should still pass.

- [ ] **Step 4: Commit**

```bash
git add crates/sunset-sync/src/engine.rs
git commit -m "$(cat <<'EOF'
sunset-sync: auto-resubscriber hook on AddPeer

When a new peer connects, replay every current BroadcastIntent as a
subscribe_via call against the new peer. Errors are logged-and-
continued (failing AddPeer because a broadcast intent failed to bind
would be worse than the inconsistency, which the next refresh tick
will surface).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 8: SUBSCRIBE_PREFIX branch in `handle_local_store_event`

**Files:**
- Modify: `crates/sunset-sync/src/engine.rs`

Provider-side handling of incoming subscription entries. When a SubscriptionEntry::Active arrives naming me as provider, populate `peer_sessions[receiver].interests` and fire `DigestRequest` for backfill. Withdrawn removes the interest.

- [ ] **Step 1: Add a filter-hash extractor**

Add a private helper near the top of `engine.rs` (or in `routing/routes.rs` if preferred — but engine-local is fine since it's only used here):

```rust
/// Extract the filter-hash component from a `_sunset-sync/subscribe/<hex>/<hex>`
/// entry name. Returns None if the name doesn't have the expected shape.
fn decode_filter_hash_from_name(name: &[u8]) -> Option<crate::routing::FilterHash> {
    let prefix = crate::routing::SUBSCRIBE_PREFIX;
    let rest = name.strip_prefix(prefix)?;
    let rest = std::str::from_utf8(rest).ok()?;
    let (hash_hex, _) = rest.split_once('/')?;
    if hash_hex.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    hex::decode_to_slice(hash_hex, &mut out).ok()?;
    Some(out)
}
```

- [ ] **Step 2: Add the new branch in handle_local_store_event**

In `handle_local_store_event` (around line 986 where the existing `if entry.name.as_ref() == reserved::SUBSCRIBE_NAME` branch lives), add immediately after that branch:

```rust
    else if entry.name.as_ref().starts_with(crate::routing::SUBSCRIBE_PREFIX) {
        let Ok(Some(block)) = self.store.get_content(&entry.value_hash).await else { return };
        let Ok(sub_entry) =
            postcard::from_bytes::<crate::routing::SubscriptionEntry>(&block.data)
        else {
            tracing::warn!(
                name = %String::from_utf8_lossy(&entry.name),
                "malformed SubscriptionEntry value; ignoring"
            );
            return;
        };
        let Some(filter_hash) = decode_filter_hash_from_name(&entry.name) else { return };
        let receiver = PeerId(entry.verifying_key.clone());
        let is_self_authored = entry.verifying_key == self.local_peer.0;

        match sub_entry {
            crate::routing::SubscriptionEntry::Active { filter, provider }
                if provider == self.local_peer =>
            {
                let was_new = {
                    let mut state = self.state.lock().await;
                    if let Some(session) = state.peer_sessions.get_mut(&receiver) {
                        session.interests.insert(filter_hash, filter.clone()).is_none()
                    } else {
                        return;
                    }
                };
                if was_new && !is_self_authored {
                    let state = self.state.lock().await;
                    if let Some(session) = state.peer_sessions.get(&receiver) {
                        let _ = session.tx.send(SyncMessage::DigestRequest {
                            filter,
                            range: DigestRange::All,
                        });
                    }
                }
            }
            crate::routing::SubscriptionEntry::Withdrawn => {
                let mut state = self.state.lock().await;
                if let Some(session) = state.peer_sessions.get_mut(&receiver) {
                    session.interests.remove(&filter_hash);
                }
            }
            // Active naming someone else: Phase 3 recursive subscription
            // will revisit (we may want to subscribe upstream). Phase 2
            // ignores.
            _ => {}
        }
    }
```

- [ ] **Step 3: Build**

Run: `nix develop --command cargo build -p sunset-sync`
Expected: success.

- [ ] **Step 4: Commit**

```bash
git add crates/sunset-sync/src/engine.rs
git commit -m "$(cat <<'EOF'
sunset-sync: handle SUBSCRIBE_PREFIX entries in handle_local_store_event

Active naming me as provider -> populate peer_sessions[r].interests +
DigestRequest. Withdrawn -> remove the interest. Self-authored or
not-naming-me entries are ignored (Phase 3 will revisit the latter
for recursive subscription).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 9: Routing tick + `republish_subscription`

**Files:**
- Modify: `crates/sunset-sync/src/engine.rs`

500 ms interval; for each `OutboundKey` in `routes.due_for_refresh`, re-publish the SubscriptionEntry::Active with a fresh priority and update `last_published_ms`.

- [ ] **Step 1: Add the republish helper**

```rust
    async fn republish_subscription(&self, key: &crate::routing::OutboundKey) -> Result<()> {
        let (filter, policy) = {
            let state = self.state.lock().await;
            let Some(ob) = state.routes.my_subs.get(key) else { return Ok(()) };
            (ob.filter.clone(), ob.policy)
        };
        self.do_subscribe_via(filter, key.provider.clone(), policy).await
    }
```

(`do_subscribe_via` already updates `last_published_ms` when it writes the entry, so the helper is a one-liner over it. The early-return guards against the entry being removed between tick scan and re-publish.)

- [ ] **Step 2: Add the routing tick to the engine run loop**

In `SyncEngine::run` (the function with the `select!` loop), add a routing-tick interval alongside the existing anti-entropy timer. Pattern after the anti-entropy code:

```rust
    let mut routing_tick = tokio::time::interval(std::time::Duration::from_millis(500));
    routing_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
```

And in the `select!`:

```rust
    _ = routing_tick.tick() => {
        let now_ms = web_time::SystemTime::now()
            .duration_since(web_time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let due = {
            let state = self.state.lock().await;
            state.routes.due_for_refresh(now_ms)
        };
        for key in due {
            if let Err(e) = self.republish_subscription(&key).await {
                tracing::warn!(?e, ?key, "subscription refresh failed");
            }
        }
    }
```

- [ ] **Step 3: Build**

Run: `nix develop --command cargo build -p sunset-sync`
Expected: success.

- [ ] **Step 4: Commit**

```bash
git add crates/sunset-sync/src/engine.rs
git commit -m "$(cat <<'EOF'
sunset-sync: routing tick + republish_subscription helper

500ms tick (Skip on miss). Per tick, due_for_refresh scan; per due
entry, re-publish via do_subscribe_via (which updates
last_published_ms). Errors per-entry are logged and we move on.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 10: Integration test — subscribe_via end-to-end

**Files:**
- Create: `crates/sunset-sync/tests/phase2_subscribe.rs`

First integration test exercises the lower layer: receiver calls `subscribe_via(filter, provider, policy)`; provider receives the entry, populates interests, fires DigestRequest; receiver eventually sees existing matching data.

- [ ] **Step 1: Skim two_peer_sync.rs for the pattern**

Run: `head -100 crates/sunset-sync/tests/two_peer_sync.rs`
Read the existing test's setup: two engines via test_transport, add_peer on both sides, the helpers it uses. Mirror that shape exactly.

- [ ] **Step 2: Write the test file**

Create `crates/sunset-sync/tests/phase2_subscribe.rs` with the first scenario, following `two_peer_sync.rs`'s structure:

```rust
//! Integration tests for Phase 2 subscribe / subscribe_via.
//!
//! Each test sets up two SyncEngines over test_transport; receiver calls
//! one of the new APIs; provider writes matching data; receiver checks
//! its local store for the data to appear.

use bytes::Bytes;
use std::time::Duration;
use sunset_store::{Filter, VerifyingKey};
use sunset_sync::routing::SubscriptionPolicy;
// ... mirror the imports from tests/two_peer_sync.rs ...

#[tokio::test]
async fn subscribe_via_backfills_existing_entry() {
    // ... follow two_peer_sync.rs setup pattern: tokio LocalSet,
    //     create two stores, two engines, connect via TestNetwork,
    //     run both engines on the LocalSet ...

    // Provider writes entry X under filter F before receiver subscribes.
    // ... insert X into provider's store ...

    // Receiver subscribes via provider for filter F.
    receiver
        .subscribe_via(filter_f.clone(), provider_peer_id.clone(), SubscriptionPolicy::store_data())
        .await
        .expect("subscribe_via");

    // Wait for X to appear in receiver's store.
    wait_for(/* receiver's store has X */).await;
}
```

(Use the existing `wait_for` helper if `two_peer_sync.rs` exports one, or copy its inline polling pattern.)

- [ ] **Step 3: Stage and run**

Run: `git add crates/sunset-sync/tests/phase2_subscribe.rs`
Run: `nix develop --command cargo test -p sunset-sync --features test-helpers --test phase2_subscribe subscribe_via_backfills_existing_entry`
Expected: pass.

- [ ] **Step 4: Commit**

```bash
git add crates/sunset-sync/tests/phase2_subscribe.rs
git commit -m "$(cat <<'EOF'
sunset-sync/tests: phase2_subscribe integration test (subscribe_via backfill)

First Phase 2 integration scenario: subscribe_via triggers backfill of
existing matching data via the existing DigestRequest path. Two
SyncEngines over test_transport; provider writes before receiver
subscribes; receiver eventually sees the entry.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 11: Integration test — `subscribe` end-to-end + auto-resubscribe

**Files:**
- Modify: `crates/sunset-sync/tests/phase2_subscribe.rs`

The high-level subscribe scenario: receiver calls `subscribe`, then a peer connects, then that peer writes matching data, then receiver sees it. Verifies the auto-resubscriber wires up subscribe_via to the new peer.

- [ ] **Step 1: Add the test**

Append to `crates/sunset-sync/tests/phase2_subscribe.rs`:

```rust
#[tokio::test]
async fn subscribe_then_new_peer_connects_then_data_flows() {
    // Setup: receiver engine running with no connected peers yet.
    // ... LocalSet, store, engine for receiver ...

    // Receiver subscribes broadcast-style.
    receiver
        .subscribe(filter_f.clone(), SubscriptionPolicy::store_data())
        .await
        .expect("subscribe");

    // Add provider peer; the auto-resubscriber should fire subscribe_via
    // for provider, which the provider's engine processes by inserting
    // into peer_sessions[receiver].interests and pushing matching data.
    // ... connect provider over test_transport, add_peer both sides ...

    // Provider writes X matching filter F.
    // ... insert X into provider's store ...

    // Receiver eventually sees X.
    wait_for(/* receiver has X */).await;
}
```

- [ ] **Step 2: Run**

Run: `nix develop --command cargo test -p sunset-sync --features test-helpers --test phase2_subscribe`
Expected: 2 tests pass.

- [ ] **Step 3: Commit**

```bash
git add crates/sunset-sync/tests/phase2_subscribe.rs
git commit -m "$(cat <<'EOF'
sunset-sync/tests: subscribe + auto-resubscribe scenario

Receiver subscribes before any peer connects; provider peer added
later; verify the auto-resubscriber fires subscribe_via on the new
peer and matching data flows.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 12: Adopt `forward_targets` at the two `peers_matching` callsites

**Files:**
- Modify: `crates/sunset-sync/src/engine.rs`

Replace the two `state.registry.peers_matching(...)` calls with `forward_targets(...)`. The legacy registry is still populated (by the existing SUBSCRIBE_NAME branch) — to keep current callers working, the function reads from BOTH: union of peers whose registry filter matches AND peers whose `PeerSession::interests` match.

For this transitional state, modify `forward_targets` (or wrap it) to also consult the registry. Simplest: do the registry check inline at the callsite, union the results with the forward_targets HashSet.

- [ ] **Step 1: Update handle_local_store_event:1054**

Find the existing block:

```rust
    let peers_to_send: Vec<PeerId> = state
        .registry
        .peers_matching(&entry.verifying_key, &entry.name)
        .collect();
```

Replace with:

```rust
    let mut peers_to_send: std::collections::HashSet<PeerId> = state
        .registry
        .peers_matching(&entry.verifying_key, &entry.name)
        .collect();
    peers_to_send.extend(crate::routing::forward_targets(
        &state.peer_sessions,
        |s| &s.interests,
        &entry.verifying_key,
        &entry.name,
    ));
    let peers_to_send: Vec<PeerId> = peers_to_send.into_iter().collect();
```

- [ ] **Step 2: Update publish_ephemeral:364**

Same pattern:

```rust
    let mut targets: std::collections::HashSet<PeerId> = state
        .registry
        .peers_matching(&datagram.verifying_key, &datagram.name)
        .collect();
    targets.extend(crate::routing::forward_targets(
        &state.peer_sessions,
        |s| &s.interests,
        &datagram.verifying_key,
        &datagram.name,
    ));
```

(Adjust to match the surrounding code's variable names and collection shape.)

- [ ] **Step 3: Build and test**

Run: `nix develop --command cargo test -p sunset-sync --features test-helpers`
Expected: pass — including existing `two_peer_sync.rs`.

- [ ] **Step 4: Commit**

```bash
git add crates/sunset-sync/src/engine.rs
git commit -m "$(cat <<'EOF'
sunset-sync: forwarding consults both legacy registry and new interests

Both peers_matching callsites union (registry.peers_matching ∪
forward_targets). HashSet absorbs the duplicate that arises when a
peer has both representations. After legacy migration (subsequent
tasks), the registry branch will be removed and forward_targets is
the sole source.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 13: Migrate 5 production callsites — `publish_subscription` → `subscribe`

**Files:**
- Modify: `crates/sunset-relay/src/relay.rs` (2 sites)
- Modify: `crates/sunset-core/src/bus.rs` (1 site)
- Modify: `crates/sunset-core/src/peer/mod.rs` (2 sites)

Mechanical rename. Drop the `Duration::from_secs(...)` ttl, replace with `SubscriptionPolicy::store_data()` (or `relay.rs`'s slower-refresh tuning).

- [ ] **Step 1: Migrate sunset-relay/src/relay.rs**

Both sites (lines 460 and 508 — `subscription_filter`-based `publish_subscription`) become:

```rust
        .subscribe(
            self.subscription_filter.clone(),
            sunset_sync::routing::SubscriptionPolicy {
                target_n: 0,
                freshness_threshold: std::time::Duration::from_secs(30),
            },
        )
        .await
```

(`target_n: 0` because the relay broadcast intent doesn't map to a per-provider count yet; Phase 3 will give this meaning. 30s threshold keeps refresh light at relay scale.)

- [ ] **Step 2: Migrate sunset-core/src/bus.rs:127**

```rust
        .subscribe(filter.clone(), sunset_sync::routing::SubscriptionPolicy::store_data())
        .await
```

- [ ] **Step 3: Migrate sunset-core/src/peer/mod.rs:103 and :128**

Both become:

```rust
        .subscribe(filter, sunset_sync::routing::SubscriptionPolicy::store_data())
        .await
```

For line 128 (with `SUBSCRIPTION_TTL`), drop the TTL constant; the policy carries refresh cadence now.

- [ ] **Step 4: Build the workspace**

Run: `nix develop --command cargo build --workspace`
Expected: success.

- [ ] **Step 5: Commit**

```bash
git add crates/sunset-relay/src/relay.rs crates/sunset-core/src/bus.rs crates/sunset-core/src/peer/mod.rs
git commit -m "$(cat <<'EOF'
sunset-{relay,core}: migrate publish_subscription to subscribe

Five production callsites now use the high-level subscribe(filter,
policy) API. Behavior is preserved: each caller wants "send me
matching entries from any directly-connected peer," which is what
subscribe expresses via the auto-resubscriber. publish_subscription
itself is still around; it's deleted in a later task once tests
also migrate.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 14: Migrate test callsites — `publish_subscription` → `subscribe`

**Files:**
- Modify: various test files

- [ ] **Step 1: Find all remaining callsites**

Run: `grep -rn "\.publish_subscription(" crates/ --include="*.rs"`
Expected: ~8 test-file callsites in `crates/sunset-sync/tests/`, `crates/sunset-core/tests/`, `crates/sunset-relay/tests/`, `crates/sunset-sync-ws-native/tests/`.

- [ ] **Step 2: Mechanically convert each**

Each call:

```rust
.publish_subscription(filter, Duration::from_secs(60))
```

becomes:

```rust
.subscribe(filter, sunset_sync::routing::SubscriptionPolicy::store_data())
```

- [ ] **Step 3: Run the whole workspace test suite**

Run: `nix develop --command cargo test --workspace --all-features --no-fail-fast`
Expected: pass.

- [ ] **Step 4: Commit**

```bash
git add crates/
git commit -m "$(cat <<'EOF'
tests: migrate publish_subscription callsites to subscribe

All test callsites converted. No semantic change; the test bodies
operate the same way.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 15: Delete `publish_subscription`, `own_filters`, and related code

**Files:**
- Modify: `crates/sunset-sync/src/engine.rs`

After this task: the legacy public API is gone. The SUBSCRIBE_NAME branch in handle_local_store_event and the registry field still exist; they're removed in Tasks 16–17.

- [ ] **Step 1: Delete the public method, the EngineCommand variant, the handler, and `do_publish_subscription`**

Remove:
- `pub async fn publish_subscription(...)` (around line 390)
- `EngineCommand::PublishSubscription { ... }` variant (around line 117)
- The arm `EngineCommand::PublishSubscription { filter, ttl, ack } => ...` (around line 559)
- `async fn do_publish_subscription(...)` (around line 1164)
- `own_filters` field on EngineState (around line 236) and its initializer in `EngineState::new` (around line 267)
- The `own_published_filters` helper and its callers in `fan_out_digests_to_peer` (around line 516–518) — if the helper has other callers, find replacements; otherwise delete.

- [ ] **Step 2: Find and update any other internal references**

Run: `grep -n "publish_subscription\|own_filters\|own_published_filters\|do_publish_subscription" crates/sunset-sync/src/engine.rs`
Expected: zero matches (besides the function declarations being removed).

- [ ] **Step 3: Build**

Run: `nix develop --command cargo build --workspace`
Expected: success.

Run: `nix develop --command cargo test --workspace --all-features --no-fail-fast`
Expected: pass.

- [ ] **Step 4: Commit**

```bash
git add crates/sunset-sync/src/engine.rs
git commit -m "$(cat <<'EOF'
sunset-sync: delete publish_subscription, own_filters, and helpers

The public API, command variant, handler, do_publish_subscription, and
the own_filters tracking are all gone. Callers have migrated.
SubscriptionRegistry and the SUBSCRIBE_NAME branch in
handle_local_store_event are still around; they're removed in
follow-up tasks once forwarding has switched fully to forward_targets
+ peer_sessions::interests.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 16: Drop the legacy SUBSCRIBE_NAME branch and the registry from forwarding

**Files:**
- Modify: `crates/sunset-sync/src/engine.rs`

After deleting publish_subscription, no live code writes SUBSCRIBE_NAME entries anymore. The registry will only ever be populated from on-disk state at bootstrap (handled in Task 18 — bootstrap stops scanning SUBSCRIBE_NAME too). Forwarding can now use `forward_targets` alone.

- [ ] **Step 1: Remove the legacy branch in handle_local_store_event**

Delete the `if entry.name.as_ref() == reserved::SUBSCRIBE_NAME { ... }` block at line 986. The SUBSCRIBE_PREFIX `else if` branch becomes a plain `if`.

- [ ] **Step 2: Drop the registry-union from forward callsites**

In both updated forwarding sites (Task 12), remove the `state.registry.peers_matching` lines and the union. Use `forward_targets` alone:

```rust
    let peers_to_send: std::collections::HashSet<PeerId> = crate::routing::forward_targets(
        &state.peer_sessions,
        |s| &s.interests,
        &entry.verifying_key,
        &entry.name,
    );
    let peers_to_send: Vec<PeerId> = peers_to_send.into_iter().collect();
```

(And the equivalent in publish_ephemeral.)

- [ ] **Step 3: Remove the `registry` field**

Drop `pub registry: SubscriptionRegistry` from `EngineState` and its initializer in `EngineState::new`. Drop the `use crate::subscription_registry::{SubscriptionRegistry, parse_subscription_entry};` import (line 18) — `parse_subscription_entry` was only used by the now-deleted SUBSCRIBE_NAME branch.

- [ ] **Step 4: Build and test**

Run: `nix develop --command cargo build --workspace`
Expected: success.

Run: `nix develop --command cargo test --workspace --all-features --no-fail-fast`
Expected: pass.

- [ ] **Step 5: Commit**

```bash
git add crates/sunset-sync/src/engine.rs
git commit -m "$(cat <<'EOF'
sunset-sync: drop SubscriptionRegistry from forwarding and EngineState

The legacy SUBSCRIBE_NAME branch in handle_local_store_event is gone.
Both peers_matching callsites use forward_targets alone. The registry
field on EngineState is removed. subscription_registry.rs is still on
disk and pub-mod'd; it's deleted in the next task.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 17: Delete `subscription_registry.rs` and `SUBSCRIBE_NAME`

**Files:**
- Delete: `crates/sunset-sync/src/subscription_registry.rs`
- Modify: `crates/sunset-sync/src/lib.rs`
- Modify: `crates/sunset-sync/src/reserved.rs`

- [ ] **Step 1: Delete the file**

Run: `git rm crates/sunset-sync/src/subscription_registry.rs`

- [ ] **Step 2: Drop the module declaration**

In `crates/sunset-sync/src/lib.rs`, remove `pub mod subscription_registry;`.

- [ ] **Step 3: Drop SUBSCRIBE_NAME from reserved.rs**

In `crates/sunset-sync/src/reserved.rs`, remove the `SUBSCRIBE_NAME` constant. Keep `is_reserved` and the `_sunset-sync/` prefix check (the new namespace still uses it). Update the `application_names_are_not_reserved` test if needed; add a test that `_sunset-sync/subscribe/anything` is reserved (covers the new prefix).

- [ ] **Step 4: Build and test**

Run: `nix develop --command cargo build --workspace`
Expected: success.

Run: `nix develop --command cargo test --workspace --all-features --no-fail-fast`
Expected: pass.

- [ ] **Step 5: Commit**

```bash
git add crates/sunset-sync/src/lib.rs crates/sunset-sync/src/reserved.rs crates/sunset-sync/src/subscription_registry.rs
git commit -m "$(cat <<'EOF'
sunset-sync: delete subscription_registry.rs and SUBSCRIBE_NAME

The file and the reserved-name constant are no longer referenced.
reserved.rs keeps is_reserved and the _sunset-sync/ prefix check
(applies to the new SUBSCRIBE_PREFIX namespace).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 18: `bootstrap_registry` → `bootstrap_routes`

**Files:**
- Modify: `crates/sunset-sync/src/engine.rs`

The existing bootstrap scans the local store for SUBSCRIBE_NAME entries and seeds the registry. It becomes `bootstrap_routes` and scans only SUBSCRIBE_PREFIX — for each `Active { provider == me }`, the entry will replay through `handle_local_store_event` once peers connect (the engine emits a per-entry replay anyway), so the bootstrap doesn't need to populate `interests` directly. It mainly serves to walk-and-warm the store for the routing-tick to find anything that should be refreshed.

- [ ] **Step 1: Rename and rewrite the function**

Find `bootstrap_registry` (around line 676). Replace with `bootstrap_routes`:

```rust
    /// Walk the local store for `_sunset-sync/subscribe/*` entries at startup.
    /// For each `Active { provider == me }`, the entry will replay through
    /// `handle_local_store_event` once peers connect; for refresh, the routing
    /// tick will pick up the corresponding `my_subs` entries that subsystems
    /// re-publish on startup (subsystems must re-call subscribe/subscribe_via
    /// after restart; persistence is a Phase 3 concern).
    ///
    /// This function exists for completeness — it currently does nothing.
    /// Phase 3+ may rehydrate `my_subs` or `broadcast_intents` here.
    async fn bootstrap_routes(&self) -> Result<()> {
        Ok(())
    }
```

Or, if the existing function is integral to startup flow, leave it as a no-op stub with the doc above. Either way, drop the SUBSCRIBE_NAME scan.

- [ ] **Step 2: Update the caller**

Wherever `bootstrap_registry()` was awaited in `run()` or `new()`, call `bootstrap_routes()` instead. The function still exists for the call symmetry; future phases will give it meat.

- [ ] **Step 3: Build and test**

Run: `nix develop --command cargo test --workspace --all-features --no-fail-fast`
Expected: pass.

- [ ] **Step 4: Commit**

```bash
git add crates/sunset-sync/src/engine.rs
git commit -m "$(cat <<'EOF'
sunset-sync: bootstrap_registry -> bootstrap_routes (no-op stub)

The old function scanned SUBSCRIBE_NAME on startup to seed the
registry. With the registry gone, the function becomes a no-op stub:
subsystems re-subscribe on startup, and Phase 3 can revisit
rehydration if a real subsystem demands it.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 19: Remaining integration tests

**Files:**
- Modify: `crates/sunset-sync/tests/phase2_subscribe.rs`

Four more scenarios from the spec.

- [ ] **Step 1: Add the four tests**

Append to `crates/sunset-sync/tests/phase2_subscribe.rs`:

```rust
#[tokio::test]
async fn unsubscribe_stops_forwarding() {
    // Setup, subscribe via P, provider writes Y, receiver sees Y.
    // Then unsubscribe(F). Provider writes Z. Receiver does NOT see Z.
    // (Poll for absence with a bounded timeout.)
}

#[tokio::test]
async fn ephemeral_datagram_uses_unified_forward_targets() {
    // Receiver subscribes via P. Provider calls publish_ephemeral with a
    // datagram matching the receiver's filter. Receiver's application
    // callback fires. Proves forward_targets is consulted by the
    // ephemeral path too.
}

#[tokio::test]
async fn peer_drop_drops_interests() {
    // Peer A connects, publishes a SubscriptionEntry naming us as provider,
    // we record A.interests. A disconnects (TestNetwork drop).
    // Assert peer_sessions no longer contains A.
    // Use SyncEngine debug helper (current_peers or similar) to observe.
}

#[tokio::test]
async fn two_receivers_one_provider() {
    // Two receiver engines, both subscribe via the same provider for
    // overlapping filters. Provider writes one matching entry; both
    // receivers see it.
}
```

(Flesh out each with the same test_transport scaffolding as the existing tests.)

- [ ] **Step 2: Run**

Run: `nix develop --command cargo test -p sunset-sync --features test-helpers --test phase2_subscribe`
Expected: 6 tests pass total.

- [ ] **Step 3: Commit**

```bash
git add crates/sunset-sync/tests/phase2_subscribe.rs
git commit -m "$(cat <<'EOF'
sunset-sync/tests: phase 2 — remaining 4 integration scenarios

unsubscribe stops forwarding; ephemeral path uses forward_targets;
peer-drop atomically drops interests; two-receiver case verifies
fan-out per receiver via separate inbound subscriptions.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 20: Final workspace verification

**Files:** none modified — this is the gate before PR.

- [ ] **Step 1: Run the full workspace tests**

Run: `nix develop --command cargo test --workspace --all-features --no-fail-fast`
Expected: all tests pass.

- [ ] **Step 2: Run clippy**

Run: `nix develop --command cargo clippy --workspace --all-features --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 3: Run fmt check**

Run: `nix develop --command cargo fmt --all --check`
Expected: clean. (If it complains, run `cargo fmt --all` and commit as a separate fixup.)

- [ ] **Step 4: Run the no-clippy-allow guard**

Run: `nix develop --command bash scripts/check-no-clippy-allow.sh`
Expected: clean (exit 0).

- [ ] **Step 5: Inspect the diff**

Run: `git log --oneline origin/master..HEAD && echo --- && git diff --stat origin/master`
Expected: ~19 commits, touching `crates/sunset-sync/`, the five production callsites, and test files. No surprise edits.

---

## Out of scope (follow-up plans)

The following are Phase 3+ and intentionally NOT in this plan:

- **Liveness ticks** (`provider-tick` publish/consume) and per-(filter, provider) freshness tracking.
- **`target_n` slot maintenance and failover.**
- **Candidate ranking** (`expected_first_data`, link-state consumption).
- **Link-state publishing.**
- **Recursive subscription** (provider that doesn't have a filter's data publishes its own subscribe_via upstream).
- **Voice subsystem integration** (`SubscriptionPolicy::voice_active_call()` activation during calls).
- **Persistence of `my_subs`/`broadcast_intents` across engine restart.**
- **Refresh-storm mitigation** for relay-scale broadcasts (a wildcard-provider entry served as N peer-pair forwards).
- **Adaptive routing-tick cadence** (next-due-timestamp instead of fixed 500 ms).
