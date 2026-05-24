# Cooperative Relay — Phase 2 Design

**Date:** 2026-05-24
**Scope:** The minimum-viable end-to-end data plane for cooperative relaying. Receivers publish per-pair `SubscriptionEntry::Active` entries via a new `subscribe_via(filter, provider, policy)` API; providers react to entries naming them and forward matching local store events to the receiver. Single-hop only (provider must already have the data; no recursive subscription upstream). Backfill on subscription arrival reuses the existing `DigestRequest`/`DigestExchange` pipeline. The legacy `SubscriptionRegistry` is retired in-memory and replaced by a unified `Routes` structure that serves both the legacy `_sunset-sync/subscribe` (SUBSCRIBE_NAME) wire path and the new `_sunset-sync/subscribe/<hash>/<provider>` per-pair wire path. Legacy public API and wire format are unchanged; existing callers are untouched.
**Out of scope (explicit):** Liveness ticks (no `_sunset-sync/provider-tick` publishing or consumption). Failover invariant — `target_n` field is recorded and ignored; no slot-filling loop. Candidate ranking and `expected_first_data`. Link-state publishing/consumption. Recursive subscription / multi-hop forwarding. Voice integration. Migration of existing `publish_subscription` callers (legacy wire path and API stay live). RAII `SubscriptionHandle` with Drop-publishes-Withdrawn. Adaptive routing-tick cadence.

## Goal

Phase 1 landed the dependency-free substrate (wire types, naming, policy, `covers()`) as dead code. Phase 2 makes that code *do something*: a new subsystem can call `engine.subscribe_via(filter, provider, policy)` and start receiving matching entries from the named provider, with backfill on subscribe and live forwarding of new entries. The legacy `publish_subscription` API and its wire format keep working without changes.

The architectural goal that earns its keep beyond just adding the API: there is now exactly *one* in-memory structure (`Routes`) that answers "given an event in my local store, which peers should I forward it to?" The legacy `SubscriptionRegistry` is retired and the two wire formats both feed `Routes`. Forwarding has one code path. This is a smaller surface than "two parallel registries with parallel fanout logic" and it sets up Phase 3 (failover, liveness) to grow within a single concept rather than across two.

## Architecture

```
Existing engine                                       NEW: Routes (sunset-sync/src/routing/routes.rs)
├── publish_subscription(filter, ttl)  ────────────►   interests[peer].legacy = Filter           (server-side cache)
│                                                                  ▲
│   self-author broadcast still propagates                         │ updated when SUBSCRIBE_NAME entry arrives
│   SUBSCRIBE_NAME entries to neighbors                            │
│
├── NEW: subscribe_via(filter, provider, policy) ──►   my_subs[(filter_hash, provider)] = Outbound {filter, policy, last_published_ms}
│                                                                  ▲
│   publish_subscription_entry writes a signed                     │ refreshed by routing-tick every ~500 ms
│   _sunset-sync/subscribe/<hash>/<provider> entry                 │
│
├── handle_local_store_event(event)        ────────►   if entry.name matches SUBSCRIBE_PREFIX:
│                                                          parse SubscriptionEntry;
│                                                          Active{filter,provider==me} → interests[author].named[hash]=filter
│                                                                                       + send DigestRequest(filter) to author
│                                                          Withdrawn                   → interests[author].named.remove(hash)
│                                                      else (any other entry):
│                                                          for peer in routes.forward_targets(vk, name): send EventDelivery(peer)
│
└── routing-tick (every ~500 ms)            ────────►   for key in routes.due_for_refresh(now_ms):
                                                            re-publish SubscriptionEntry::Active for key
                                                            routes.my_subs[key].last_published_ms = now_ms

DigestRequest / DigestExchange / diff / push: unchanged.   Phase 1 substrate (SubscriptionEntry, LinkState, ProviderTick, SubscriptionPolicy, covers): unchanged.
```

The legacy `SubscriptionRegistry` type and its `subscription_registry.rs` file are deleted. Tests against it are rewritten against `Routes`. Behavior is preserved — same wire format, same fanout decisions, same DigestRequest emission on SUBSCRIBE_NAME changes.

## The `Routes` data structure

`crates/sunset-sync/src/routing/routes.rs` (new):

```rust
use std::collections::HashMap;

use sunset_store::{Filter, VerifyingKey};

use crate::routing::policy::SubscriptionPolicy;
use crate::types::PeerId;

/// Type alias for clarity at use sites; opaque 32-byte blake3 hash.
pub type FilterHash = [u8; 32];

/// Unified routing state: my own subscriptions (outgoing) plus an index
/// of what every other peer wants from me (incoming, unifying the
/// legacy SUBSCRIBE_NAME path and the new per-pair path).
pub struct Routes {
    me: PeerId,
    /// My own subscriptions (Phase 2 new path). One entry per (filter, provider) pair.
    /// Legacy `publish_subscription` calls populate `EngineState.own_filters`
    /// separately and are NOT reflected here.
    pub my_subs: HashMap<OutboundKey, Outbound>,
    /// What every other peer wants from me. Replaces SubscriptionRegistry.
    /// Entries removed wholesale when the peer disconnects (existing
    /// peer-drop handler in engine.rs).
    pub interests: HashMap<PeerId, PeerInterests>,
}

#[derive(Clone, Debug, Hash, Eq, PartialEq)]
pub struct OutboundKey {
    pub filter_hash: FilterHash,
    pub provider: PeerId,
}

#[derive(Clone, Debug)]
pub struct Outbound {
    pub filter: Filter,
    pub policy: SubscriptionPolicy,
    pub last_published_ms: u64,
}

#[derive(Clone, Debug, Default)]
pub struct PeerInterests {
    /// From this peer's `_sunset-sync/subscribe` entry (SUBSCRIBE_NAME path).
    /// At most one filter per peer; replaced wholesale on each update.
    pub legacy: Option<Filter>,
    /// From this peer's `_sunset-sync/subscribe/<hash>/<me>` entries.
    /// Keyed by filter_hash so Withdrawn (which only carries the hash via
    /// the entry name) is an O(1) lookup; the value carries the filter for
    /// matching purposes.
    pub named: HashMap<FilterHash, Filter>,
}

impl Routes {
    pub fn new(me: PeerId) -> Self {
        Self { me, my_subs: HashMap::new(), interests: HashMap::new() }
    }

    /// "Given an event with this (verifying_key, name), which peers should
    /// I forward to?" Replaces SubscriptionRegistry::peers_matching at both
    /// existing callsites. A peer subscribed via both legacy and new paths
    /// appears once (the function returns deduplicated PeerIds).
    pub fn forward_targets(&self, vk: &VerifyingKey, name: &[u8]) -> Vec<PeerId> {
        let mut out = Vec::new();
        for (peer, pi) in &self.interests {
            let matched = pi.legacy.as_ref().is_some_and(|f| f.matches(vk, name))
                || pi.named.values().any(|f| f.matches(vk, name));
            if matched {
                out.push(peer.clone());
            }
        }
        out
    }

    /// Returns my_subs entries whose refresh is due. An entry is due when
    /// `now_ms - last_published_ms >= policy.freshness_threshold / 2`.
    /// (Half the freshness threshold is the simple Nyquist-fast choice for
    /// Phase 2; an adaptive cadence comes with the liveness phase.)
    pub fn due_for_refresh(&self, now_ms: u64) -> Vec<OutboundKey> {
        self.my_subs
            .iter()
            .filter(|(_, ob)| {
                now_ms.saturating_sub(ob.last_published_ms)
                    >= (ob.policy.freshness_threshold.as_millis() as u64) / 2
            })
            .map(|(k, _)| k.clone())
            .collect()
    }

}
```

Two non-trivial queries (`forward_targets`, `due_for_refresh`). Everything else is direct HashMap access; the engine reads/writes the fields it needs without method-soup. A `has_any_interest` predicate or similar isn't introduced — peer drop just removes the whole `interests[peer]` entry, which is O(1) regardless.

### Why `legacy: Option<Filter>` (not `Vec<Filter>`)

The existing legacy path packs all subsystems' filters into a single `Filter::Union(...)` at the wire layer (engine.rs accumulates `own_filters` and publishes one union). On receive, parsing yields one (possibly-Union) `Filter`. Storing `Option<Filter>` mirrors the wire shape exactly. No change to the legacy semantics; the engine's existing `do_publish_subscription` continues to produce the same on-wire entry.

### Why two backing maps inside `PeerInterests` rather than a tagged enum or merged set

The two paths arrive via different wire events (SUBSCRIBE_NAME vs. SUBSCRIBE_PREFIX names) and have different removal semantics: legacy is replaced wholesale on each SUBSCRIBE_NAME update; new is per-pair add/remove keyed by filter_hash. Modeling them as one map with tagged keys requires the same two access paths anyway. Keeping them as separate fields lets each callsite name the slot it cares about (legacy vs. named) at use, which is more honest about the wire-level distinction than "tag everything `LegacyOrigin | NamedOrigin`."

## Engine integration

All changes are in `crates/sunset-sync/src/engine.rs` (and the unification deletes `subscription_registry.rs`).

### EngineState

```rust
pub struct EngineState<St: Store> {
    // ... existing fields, less `pub registry: SubscriptionRegistry` ...

    /// Unified routing state — replaces `registry`. Persists across engine restarts
    /// only insofar as it can be rebuilt from `_sunset-sync/subscribe*` entries on
    /// disk (`bootstrap_registry` becomes `bootstrap_routes`).
    pub routes: routing::Routes,
}
```

### Two new EngineCommand variants

```rust
enum EngineCommand {
    // ... existing variants ...
    Subscribe {
        filter: Filter,
        provider: PeerId,
        policy: SubscriptionPolicy,
        ack: oneshot::Sender<Result<()>>,
    },
    Unsubscribe {
        filter: Filter,
        provider: PeerId,
        ack: oneshot::Sender<Result<()>>,
    },
}
```

Public methods on `SyncEngine`:

```rust
pub async fn subscribe_via(
    &self, filter: Filter, provider: PeerId, policy: SubscriptionPolicy,
) -> Result<()>;
pub async fn unsubscribe_via(
    &self, filter: Filter, provider: PeerId,
) -> Result<()>;
```

Naming mirrors `publish_subscription` while making the provider parameter explicit. Both methods are fire-and-forget from the caller's perspective: on `Ok`, the entry has been persisted; refresh and forwarding happen autonomously.

### Subscribe path

`do_subscribe_via(filter, provider, policy)`:

1. Compute `filter_hash = blake3(postcard(filter))`.
2. Build the `SubscriptionEntry::Active { filter, provider }` value and the entry name via `routing::subscription_name(&filter, &provider)`.
3. Sign and insert the entry into the local store. This rides the existing self-author broadcast: the engine sees its own entry in `handle_local_store_event` and fans it out to every directly connected peer. (Self-author broadcast is unchanged — it currently sends all self-authored entries to all `peer_outbound` channels regardless of filter.)
4. Insert into `routes.my_subs` with `last_published_ms = now`.
5. Acknowledge.

`do_unsubscribe_via(filter, provider)`:

1. Compute the key. If not in `my_subs`, return `Ok(())` (idempotent withdraw).
2. Publish `SubscriptionEntry::Withdrawn` at the same name. TTL ≥ the previous active entry's so the withdrawal propagates rather than being immediately GC'd. (Practical implementation: use the active entry's original TTL; precise tracking is a Phase 3 concern.)
3. Remove from `routes.my_subs`.
4. Acknowledge.

### handle_local_store_event branches

Two branches added; the legacy SUBSCRIBE_NAME branch is updated to write to `routes.interests[peer].legacy` instead of `state.registry.insert(...)`.

```rust
// Existing branch, updated to write to the unified store:
if entry.name.as_ref() == reserved::SUBSCRIBE_NAME {
    if let Ok(Some(block)) = self.store.get_content(&entry.value_hash).await {
        if let Ok(filter) = parse_subscription_entry(&entry, &block) {
            let peer_vk = entry.verifying_key.clone();
            let peer_id = PeerId(peer_vk.clone());
            let prev = {
                let mut state = self.state.lock().await;
                let entry = state.routes.interests.entry(peer_id.clone()).or_default();
                let prev = entry.legacy.clone();
                entry.legacy = Some(filter.clone());
                prev
            };
            let filter_changed = prev.as_ref() != Some(&filter);
            let is_self = peer_vk == self.local_peer.0;
            if filter_changed && !is_self {
                // Unchanged: fire DigestRequest for backfill.
                let msg = SyncMessage::DigestRequest {
                    filter: filter.clone(), range: DigestRange::All,
                };
                let state = self.state.lock().await;
                if let Some(po) = state.peer_outbound.get(&peer_id) {
                    let _ = po.tx.send(msg);
                }
            }
        }
    }
}

// New branch for the per-pair path:
else if entry.name.as_ref().starts_with(routing::SUBSCRIBE_PREFIX) {
    let Ok(Some(block)) = self.store.get_content(&entry.value_hash).await else { return };
    let Ok(sub_entry) = postcard::from_bytes::<SubscriptionEntry>(&block.data) else {
        tracing::warn!("malformed SubscriptionEntry at {}", String::from_utf8_lossy(&entry.name));
        return;
    };
    let filter_hash = decode_filter_hash_from_name(&entry.name)?;
    let receiver = PeerId(entry.verifying_key.clone());
    let is_self_authored = entry.verifying_key == self.local_peer.0;

    match sub_entry {
        SubscriptionEntry::Active { filter, provider } if provider == self.local_peer => {
            // This entry names me as provider. Track it and trigger backfill.
            let was_new = {
                let mut state = self.state.lock().await;
                let pi = state.routes.interests.entry(receiver.clone()).or_default();
                pi.named.insert(filter_hash, filter.clone()).is_none()
            };
            if was_new && !is_self_authored {
                let msg = SyncMessage::DigestRequest {
                    filter, range: DigestRange::All,
                };
                let state = self.state.lock().await;
                if let Some(po) = state.peer_outbound.get(&receiver) {
                    let _ = po.tx.send(msg);
                }
            }
        }
        SubscriptionEntry::Withdrawn => {
            let mut state = self.state.lock().await;
            if let Some(pi) = state.routes.interests.get_mut(&receiver) {
                pi.named.remove(&filter_hash);
            }
        }
        // Active naming someone else: Phase 2 ignores. Phase 3 recursive
        // subscription will revisit (we may want to subscribe upstream).
        _ => {}
    }
}
```

The two existing callsites of `state.registry.peers_matching` (`publish_ephemeral`, line ~364, and the non-self fanout in `handle_local_store_event`, line ~1054) become `state.routes.forward_targets(&vk, &name)`. Same return value; deduplicated peer list.

### Routing tick

A new `tokio::time::interval(Duration::from_millis(500))` runs on the engine's `LocalSet`, alongside the existing anti-entropy timer. Per tick:

```rust
let due = {
    let state = self.state.lock().await;
    state.routes.due_for_refresh(now_ms())
};
for key in due {
    if let Err(e) = self.republish_subscription(&key).await {
        tracing::warn!(?e, ?key, "subscription refresh failed");
    }
}
```

`republish_subscription` builds the same signed entry as the initial subscribe (with a fresh priority and TTL extension), inserts it, and updates `last_published_ms`. LWW collapses the new entry onto the same key. Failure (signing or store error) is logged and the tick continues — the next tick will retry.

500 ms is Nyquist-fast against the only Phase 2 policy in play (`store_data()`, 5 s threshold → refresh every ~2.5 s). Voice-active-call's 200 ms threshold would need ≤100 ms tick cadence; that's a Phase 5 problem.

### Bootstrap (engine startup)

The existing `bootstrap_registry` walks the local store for SUBSCRIBE_NAME entries and seeds `state.registry`. It becomes `bootstrap_routes` and seeds both halves of `state.routes.interests`:

1. Iterate `Filter::Namespace(SUBSCRIBE_NAME)` → for each, set `interests[peer].legacy`.
2. Iterate `Filter::NamePrefix(SUBSCRIBE_PREFIX)` → for each, parse `SubscriptionEntry`.
   - `Active { filter, provider == me }` → set `interests[author].named[hash] = filter`.
   - `Active { filter, provider != me }` → ignore (this entry names someone else; Phase 2 has no role for it).
   - `Withdrawn` → ignore (the entry exists only to propagate the withdrawal; no live state to restore).

Both `routes.my_subs` and the receiver-side rejoin of in-flight subscriptions are NOT rehydrated from disk in Phase 2. A process restart re-subscribes on demand. This is OK because subscribe is cheap and `my_subs` is purely a local cache for refresh scheduling; the on-wire entry survives via the store's TTL window. Phase 3+ can revisit if the rehydration latency matters.

### Peer drop

When `peer_outbound` loses a peer (existing handler around line 568), also call `state.routes.interests.remove(&peer_id)`. This drops both legacy and named entries for that peer in one shot. The peer's published subscriptions remain in the local store until they expire by TTL — which is what we want; if the peer reconnects within the TTL, the entries are still there.

## Forwarding semantics

For every entry that arrives in the local store (`handle_local_store_event` continues to be the central dispatch), the existing two-rule decision tree remains:

- **Self-authored** → broadcast to every directly connected peer in `peer_outbound`, no filter check. (Bootstrap-safe; unchanged from today.)
- **Authored by someone else** → iterate `state.routes.forward_targets(&vk, &name)`, send `EventDelivery` to each. (Replaces the iteration over `state.registry.peers_matching`.)

A receiver that subscribed via both the legacy path AND the new path appears once in `forward_targets`. The receiver gets one `EventDelivery`. No duplicate-bandwidth wart from coexistence at the provider; the CRDT-idempotency safety net is for the rarer cross-provider case (two different providers happen to push the same entry to the same receiver — common during failover transitions in later phases).

## Backfill

When a new subscription entry arrives at the provider (either path), the engine sends `SyncMessage::DigestRequest { filter, range: DigestRange::All }` to the subscribing peer. The peer responds with a `DigestExchange` bloom; the provider diffs, pushes missing entries as `EventDelivery`. This is the unmodified existing pipeline (engine.rs:1007 for legacy, mirrored in the new branch above).

Bloom dedup ensures the receiver only gets entries it doesn't already have. If the receiver subscribed via both legacy and new paths simultaneously, two DigestRequests fire and two exchanges run — the second yields no new entries via bloom dedup. Wasted CPU, no wasted bandwidth, no correctness issue. Phase 3 can deduplicate per `(receiver, filter)` if profiling shows this matters.

## Testing

Two layers, both following existing conventions.

**Unit tests on `Routes`** (in `routing/routes.rs` `#[cfg(test)]` block):
- `forward_targets` returns the expected peers for legacy-only / named-only / both / neither cases.
- `forward_targets` deduplicates a peer subscribed via both paths.
- `due_for_refresh` returns only entries whose `last_published_ms` is sufficiently old.
- `has_any_interest` reflects legacy and named correctly.
- Insertion/removal at the unified API leaves the structure consistent.

**Integration tests** under `crates/sunset-sync/tests/` (new file `phase2_subscribe_via.rs`, follows `two_peer_sync.rs` shape):
1. **Existing-data backfill.** Provider writes entry X. Receiver calls `subscribe_via(filter_matching_X, provider, store_data())`. Receiver eventually sees X.
2. **Future-data forwarding.** Receiver subscribes; then provider writes Y matching the filter. Receiver sees Y via the new forwarding path.
3. **Unsubscribe stops forwarding.** Receiver subscribes, sees Y, unsubscribes. Provider writes Z. Receiver does NOT see Z.
4. **Two receivers, one provider.** Both subscribe to overlapping filters. Provider writes one matching entry. Both receivers see it.
5. **Two filters from one receiver.** Receiver subscribes via the same provider with two distinct filters. Each delivers independently.
6. **Coexistence with legacy.** Receiver uses both `publish_subscription` and `subscribe_via`. No double-delivery (provider's `forward_targets` deduplicates). No errors. Existing `two_peer_sync.rs` test continues to pass without modification.

The conformance suite is unaffected (it tests `Store`, not `SyncEngine`). Existing `subscribe_backfill.rs` and `two_peer_sync.rs` integration tests must continue to pass; they will need touch-ups only if they referenced `state.registry` directly, which they don't (they exercise the public API).

## Coexistence and migration story

After this PR:
- Existing subsystems (presence, voice signaling, room subscriptions) keep using `engine.publish_subscription(filter, ttl)`. Their wire format is unchanged.
- New code can opt into `engine.subscribe_via(filter, provider, policy)` per call site, choosing a provider explicitly. Phase 2 has no such caller; tests are the only consumers.
- The forwarding decision is unified — `Routes::forward_targets` is the one function that decides "send to whom."

A future Phase 6 (or whenever it makes sense) migrates each existing caller from `publish_subscription` to `subscribe_via`. At that point the legacy wire path (`SUBSCRIBE_NAME` reserved name) can be retired and `PeerInterests::legacy` becomes dead code. None of that lands in Phase 2.

## What stays the same

- Wire format of every existing entry type. `SUBSCRIBE_NAME` continues to carry a postcard'd `Filter` value.
- Public API (`publish_subscription`, `add_peer`, every existing engine method).
- Self-author broadcast behavior (broadcast to all connected peers regardless of filter).
- `DigestRequest`/`DigestExchange`/diff/push pipeline.
- Phase 1 substrate (`SubscriptionEntry`, `LinkState`, `Neighbor`, `ProviderTick`, `SubscriptionPolicy`, `covers`, `subscription_name`, reserved-name constants) — `LinkState`, `Neighbor`, `ProviderTick`, and `covers` remain unused dead code waiting for Phase 3+.
- Store contract, transport stack, crypto, voice frame format.

## What's new

- `crates/sunset-sync/src/routing/routes.rs` (new ~100 LOC + tests).
- `crates/sunset-sync/src/routing/mod.rs` — add `pub mod routes; pub use routes::{Routes, OutboundKey, Outbound, PeerInterests, FilterHash};`.
- `crates/sunset-sync/src/engine.rs`:
  - Replace `pub registry: SubscriptionRegistry` with `pub routes: routing::Routes` in `EngineState`.
  - Add two `EngineCommand` variants + two `pub async fn` methods.
  - Add one new branch in `handle_local_store_event` for the SUBSCRIBE_PREFIX namespace; update the existing SUBSCRIBE_NAME branch to write `interests[peer].legacy`.
  - Replace 2 callsites of `state.registry.peers_matching` with `state.routes.forward_targets`.
  - Replace `state.registry.insert(...)` / `state.registry.remove(...)` calls with the corresponding `routes.interests` operations.
  - Rename `bootstrap_registry` → `bootstrap_routes`; extend to also scan `SUBSCRIBE_PREFIX`.
  - Add a new routing tick alongside the anti-entropy timer.
- `crates/sunset-sync/src/subscription_registry.rs` — deleted. Its `parse_subscription_entry` helper (used to decode the legacy SUBSCRIBE_NAME wire value into a `Filter`) is a ~10-line postcard wrapper used in exactly one place; inline it into `engine.rs`'s SUBSCRIBE_NAME branch rather than spawning a one-call-site helper module.
- `crates/sunset-sync/src/lib.rs` — drop `pub mod subscription_registry;`.
- `crates/sunset-sync/tests/phase2_subscribe_via.rs` (new) — six integration scenarios.

## Open questions (deferred, not blocking)

- **Persistence of `my_subs` across engine restart.** Phase 2 rebuilds on demand from subscribe calls. If a subsystem expects subscriptions to survive restart automatically, it must re-call subscribe on startup. Phase 3 (or earlier if a real subsystem demands it) can rehydrate from `_sunset-sync/subscribe/*` entries authored by self on disk.
- **Exact TTL for Withdrawn entries.** Spec says "≥ the previous active entry's"; implementation uses the original TTL value. Tracking the previous entry's `expires_at` precisely is a Phase 3 nicety.
- **Adaptive routing-tick cadence.** Fixed 500 ms tick. Voice integration in Phase 5 requires sub-200 ms; tick becomes "fire at next-due timestamp."
- **De-duplicating DigestRequest fires** when a receiver subscribed via both paths.
