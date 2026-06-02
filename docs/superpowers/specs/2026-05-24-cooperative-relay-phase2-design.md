# Cooperative Relay — Phase 2 Design

**Date:** 2026-05-24
**Scope:** Land the data plane for cooperative relaying as two layered APIs over a single mechanism. `subscribe_via(filter, provider, policy)` is the mechanism — directed at one specific peer. `subscribe(filter, policy)` is the policy layer — declare interest in a filter without naming the provider, implemented as an auto-resubscriber that calls `subscribe_via` for each directly-connected peer. One wire format end to end. The pre-1.0 `publish_subscription` API and the `_sunset-sync/subscribe` (SUBSCRIBE_NAME) wire entry retire; `SubscriptionRegistry` and `own_filters` are deleted. The five existing call sites migrate. Forwarding has one decision function shared by reliable `EventDelivery` and ephemeral `SignedDatagram` paths.
**Out of scope (explicit):** Liveness ticks (`provider-tick` publish/consume). `target_n`-based slot maintenance and failover. Candidate ranking and `expected_first_data`. Link-state publishing/consumption. Recursive (multi-hop) subscription. Voice integration. RAII `SubscriptionHandle` with Drop-publishes-Withdrawn. Adaptive routing-tick cadence.

## Goal

Phase 1 landed the wire types, naming, policy, and `covers()` as dead code. Phase 2 wires them in. The structural goal is *one shape*: one wire format for subscriptions, one storage layout for the in-memory cache, one function that answers "given this event, who should I forward it to?". The two API methods are the two *layers* over that mechanism, not two parallel paths.

The reason the layered framing matters: when Phase 3 adds liveness and ranking, both new behaviors layer over `subscribe_via` cleanly (failover changes what providers `subscribe` picks; liveness changes which providers are considered alive). If we'd shipped two parallel paths, every later layer would have to retrofit both.

## The two layers

```
Application code
       │
       ├─ engine.subscribe(filter, policy)        ──┐
       │                                            │   "I want F from anyone connected."
       │                                            │   Auto-resubscriber: on every peer
       │                                            │   currently connected, AND on every
       │                                            │   future connect, call subscribe_via
       │                                            ▼   for this filter.
       │
       ├─ engine.subscribe_via(filter, provider, policy)
       │                                            │   "I want F from this specific peer."
       │                                            │   Writes one signed SubscriptionEntry
       │                                            │   at _sunset-sync/subscribe/<hash>/<provider>.
       │                                            │   Refreshes on the routing tick.
       │                                            ▼
       │
Engine ┴────────────────────────────────────────►  Routes + per-peer state
                                                   (one mechanism)
```

`subscribe` is a *policy* implemented in terms of `subscribe_via`. There is no separate "broadcast" wire path. There is no separate inbound storage for broadcast vs. directed subscriptions. The receiver sees exactly the same entry shape regardless of which API the sender used; the only difference is *how many* such entries get written (one per connected peer for `subscribe`; exactly one for `subscribe_via`).

## Wire format

One entry shape, exactly as Phase 1 specified:

- **Name:** `_sunset-sync/subscribe/<blake3(postcard(filter))_hex>/<provider_pubkey_hex>`
- **Value:** `SubscriptionEntry::Active { filter, provider }` or `SubscriptionEntry::Withdrawn`
- **Signed by:** the receiver (the peer who wants the data)

The legacy `_sunset-sync/subscribe` (SUBSCRIBE_NAME) key with bare `Filter` payload no longer exists. `reserved::SUBSCRIBE_NAME` is deleted. Existing on-disk entries at that key are ignored (the engine no longer recognizes them); they expire by TTL.

## In-memory state

Engine state grows by two changes:

**Change 1.** `peer_outbound: HashMap<PeerId, PeerOutbound>` becomes `peer_sessions: HashMap<PeerId, PeerSession>`. The struct adds an `interests` field for what this peer wants from me. This keeps everything peer-keyed in one place; peer drop is one `remove()` call.

```rust
pub struct PeerSession {
    pub tx: UnboundedSender<SyncMessage>,
    pub kind: TransportKind,
    /// What this peer currently wants from me. Keyed by filter_hash for O(1)
    /// Withdrawn lookups (the entry name carries the hash, not the filter).
    pub interests: HashMap<FilterHash, Filter>,
}
```

**Change 2.** A new `routes: Routes` field replaces both `subscription_registry` and `own_filters`. A new type alias `FilterHash` (introduced in this PR alongside `Routes`) names the 32-byte blake3 hash of a postcard-encoded filter; it's the key shape both `my_subs` and `PeerSession::interests` use:

```rust
/// 32-byte blake3 hash of postcard(filter). Used as a key wherever the
/// filter itself would be redundant or would force a `Hash` impl on `Filter`
/// (which the store doesn't currently provide).
pub type FilterHash = [u8; 32];

pub struct Routes {
    me: PeerId,
    /// My outgoing subscription entries — one per (filter, provider) pair I've
    /// asked. Auto-resubscriber and subscribe_via both write here; the routing
    /// tick refreshes from here.
    pub my_subs: HashMap<OutboundKey, Outbound>,
    /// High-level intents from `subscribe(filter, policy)`. The auto-resubscriber
    /// reads this to decide what to subscribe-via on each peer connect, and to
    /// know what to tear down on `unsubscribe(filter)`.
    pub broadcast_intents: HashMap<FilterHash, BroadcastIntent>,
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

#[derive(Clone, Debug)]
pub struct BroadcastIntent {
    pub filter: Filter,
    pub policy: SubscriptionPolicy,
}
```

Two non-trivial queries earn methods; everything else is direct field access:

```rust
impl Routes {
    pub fn new(me: PeerId) -> Self;

    /// Returns my_subs entries whose refresh is due.
    pub fn due_for_refresh(&self, now_ms: u64) -> Vec<OutboundKey>;
}

/// Forwarding decision — which peers should I send this event to?
/// Free function (not a Routes method) because the data lives in `peer_sessions`,
/// not `Routes`. Returns HashSet so the dedup contract is in the type.
pub fn forward_targets(
    peer_sessions: &HashMap<PeerId, PeerSession>,
    vk: &VerifyingKey,
    name: &[u8],
) -> HashSet<PeerId>;
```

### Why interests live in `PeerSession`, not in `Routes`

Today, the V2 audit caught: "the next person adding a peer-drop site has to remember to also clean up interests." That's the `update_a(); update_b();` anti-pattern. With `interests` as a field of `PeerSession`, peer disconnect is *one* `peer_sessions.remove(&peer_id)` operation — interests get dropped automatically because they're owned by the session. The structural invariant ("interests outlive the session = stale state") becomes impossible by construction.

### Why `broadcast_intents` is separate from `my_subs`

`my_subs` is wire state: one entry per `(filter, provider)` actually published. `broadcast_intents` is *intent* state: one entry per `subscribe(filter)` call. The auto-resubscriber transforms intents into wire entries as peers come and go. Combining them into one map would require a sentinel or tag to distinguish "intent without wire entry yet" vs. "wire entry without intent" — a denormalization smell. Keeping them separate makes each one say exactly what it is.

## Engine API

Two new methods on `SyncEngine`. `publish_subscription` is deleted.

```rust
impl SyncEngine {
    /// Declare interest in a filter from any directly-connected peer.
    /// Implemented as an auto-resubscriber: for each peer in peer_sessions
    /// now, calls subscribe_via(filter, peer, policy). On every future peer
    /// connect, the engine calls subscribe_via for each broadcast_intent.
    pub async fn subscribe(
        &self, filter: Filter, policy: SubscriptionPolicy,
    ) -> Result<()>;

    /// Cancel a subscribe() — tear down the broadcast_intent and publish
    /// Withdrawn for every my_subs entry the intent produced.
    pub async fn unsubscribe(&self, filter: Filter) -> Result<()>;

    /// Ask a specific peer for matching entries. The mechanism layer.
    pub async fn subscribe_via(
        &self, filter: Filter, provider: PeerId, policy: SubscriptionPolicy,
    ) -> Result<()>;

    /// Withdraw one specific (filter, provider) subscription.
    pub async fn unsubscribe_via(
        &self, filter: Filter, provider: PeerId,
    ) -> Result<()>;
}
```

**Revision (2026-05-24):** `policy.target_n` has been removed from `SubscriptionPolicy` in Phase 2. The original Phase 2 design recorded `target_n` without interpreting it (`subscribe` is unconditionally "all connected"), but in practice no caller branched on it and the `relay_broad()` constructor was shipping `target_n: 0` as an undefined sentinel. Per the module's anti-pattern doc, adding knobs without consumers re-introduces the enumerated-cases-as-algorithm smell. The slot-maintenance knob will be re-introduced in Phase 3 when a real caller anchors the choice between per-policy slot count vs. a per-`subscribe_via` argument. Phase 2's `SubscriptionPolicy` is exactly `freshness_threshold` plus the `entry_ttl()` / `refresh_interval()` derivations.

Backed by four new `EngineCommand` variants (`Subscribe`, `Unsubscribe`, `SubscribeVia`, `UnsubscribeVia`).

## Subscribe path (the inside of each method)

**`do_subscribe(filter, policy)`:**
1. Insert into `routes.broadcast_intents` (idempotent — re-subscribing is a no-op).
2. For each peer currently in `peer_sessions`: call `do_subscribe_via(filter.clone(), peer.clone(), policy)`.

**`do_subscribe_via(filter, provider, policy)`:**
1. Compute `filter_hash = blake3(postcard(filter))`.
2. Build the `SubscriptionEntry::Active { filter, provider }` value and the entry name via `routing::subscription_name(&filter, &provider)`.
3. Sign and insert the entry into the local store. Self-author broadcast (existing engine behavior) propagates it to every connected peer.
4. Insert into `routes.my_subs` with `last_published_ms = now`.

**`do_unsubscribe(filter)`:**
1. Remove from `routes.broadcast_intents`. If absent, return `Ok(())`.
2. For every `OutboundKey` in `my_subs` whose `filter_hash` matches: call `do_unsubscribe_via`.

**`do_unsubscribe_via(filter, provider)`:**
1. Compute key; if absent from `my_subs`, return `Ok(())`.
2. Build a `SubscriptionEntry::Withdrawn` value and publish at the same name with TTL ≥ the active entry's so it propagates.
3. Remove from `my_subs`.

## Auto-resubscriber

The auto-resubscriber lives in the engine's existing peer-lifecycle handlers, not in `Routes`. (`Routes` is data; lifecycle is engine concern.)

**On peer connect** (in the existing `EngineCommand::AddPeer` path, after `peer_sessions.insert`):

```rust
let intents: Vec<BroadcastIntent> = state.routes.broadcast_intents.values().cloned().collect();
drop(state);  // release the lock; subscribe_via re-acquires
for intent in intents {
    self.do_subscribe_via(intent.filter, peer_id.clone(), intent.policy).await?;
}
```

**On peer drop** (in the existing peer-drop path, `peer_sessions.remove(&peer_id)` becomes one operation):

The session is gone, so its interests vanish with it. My own `my_subs` entries targeting this peer become un-deliverable; let them expire by TTL. (The peer can't acknowledge a Withdrawn anyway, and re-connecting peers will see the re-emitted entries from the next `subscribe` cycle.)

Phase 3's per-(filter, provider) liveness will revisit "should we proactively withdraw my_subs for dropped peers, or keep them for fast rebind on quick reconnect." Phase 2 just lets them age out.

## handle_local_store_event

One updated branch for the subscription namespace, replacing the existing legacy SUBSCRIBE_NAME branch:

```rust
if entry.name.as_ref().starts_with(routing::SUBSCRIBE_PREFIX) {
    let Ok(Some(block)) = self.store.get_content(&entry.value_hash).await else { return };
    let Ok(sub_entry) = postcard::from_bytes::<SubscriptionEntry>(&block.data) else {
        tracing::warn!("malformed SubscriptionEntry at {}", String::from_utf8_lossy(&entry.name));
        return;
    };
    let Some(filter_hash) = decode_filter_hash_from_name(&entry.name) else { return };
    let receiver = PeerId(entry.verifying_key.clone());
    let is_self_authored = entry.verifying_key == self.local_peer.0;

    match sub_entry {
        SubscriptionEntry::Active { filter, provider } if provider == self.local_peer => {
            let was_new = {
                let mut state = self.state.lock().await;
                if let Some(session) = state.peer_sessions.get_mut(&receiver) {
                    session.interests.insert(filter_hash, filter.clone()).is_none()
                } else {
                    // Receiver isn't connected to us right now; nothing to do.
                    // Their entry stays in the store; if they reconnect, the
                    // entry replays and this branch fires again.
                    return;
                }
            };
            if was_new && !is_self_authored {
                let state = self.state.lock().await;
                if let Some(session) = state.peer_sessions.get(&receiver) {
                    let _ = session.tx.send(SyncMessage::DigestRequest {
                        filter, range: DigestRange::All,
                    });
                }
            }
        }
        SubscriptionEntry::Withdrawn => {
            let mut state = self.state.lock().await;
            if let Some(session) = state.peer_sessions.get_mut(&receiver) {
                session.interests.remove(&filter_hash);
            }
        }
        // Active naming someone else: Phase 3 recursive subscription will
        // revisit (we may want to subscribe upstream). Phase 2 ignores.
        _ => {}
    }
}
```

Everything else in `handle_local_store_event` is unchanged. The other-author fanout (line ~1054 today) calls `forward_targets(&state.peer_sessions, &vk, &name)` instead of `state.registry.peers_matching(...)`.

## Forwarding semantics — reliable and ephemeral share the path

`forward_targets` is consulted by *two* callsites today, both keep the same role after this PR:

- `handle_local_store_event` (engine.rs:1054 area) — non-self entries fan out to `EventDelivery` recipients.
- `publish_ephemeral` (engine.rs:364 area) — `SignedDatagram`s fan out to `EphemeralDelivery` recipients.

Same routing decision, two message types on the wire. The receiver's ingest differs by message type (reliable inserts into the local store; ephemeral calls the application callback) but the *who-do-I-send-to* function is one piece of code shared by both.

## Backfill — reliable only

When a new `SubscriptionEntry::Active` lands at a provider naming them, the provider sends `SyncMessage::DigestRequest { filter, range: DigestRange::All }` to the receiver. The receiver responds with a `DigestExchange` bloom; the provider diffs against its local store and pushes missing entries as `EventDelivery`. This is the unmodified existing `DigestRequest`/`DigestExchange`/diff/push pipeline.

Ephemeral data is not stored, so it has nothing to backfill. A new subscription's ephemeral data plane starts at "future datagrams only" — the same way a reader who turns on the radio mid-broadcast doesn't get a replay of the morning show. Correct behavior; worth being explicit about.

## Routing tick

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

`republish_subscription` rebuilds the signed entry (same value, fresh priority and TTL), inserts it, updates `last_published_ms`. LWW collapses to the same key.

`due_for_refresh` returns entries where `now_ms - last_published_ms >= policy.freshness_threshold / 2` — Nyquist-fast for the only Phase 2 policy in play (`store_data()`, 5s threshold → refresh every ~2.5s). 500ms is fine; adaptive cadence is Phase 5's problem when voice (200ms threshold) lands.

## Bootstrap

The existing `bootstrap_registry` is replaced by `bootstrap_routes`. On engine startup, it scans the local store for `_sunset-sync/subscribe/*` entries (using `Filter::NamePrefix(SUBSCRIBE_PREFIX)`) and, for each:

- `Active { filter, provider == me }` → record into the appropriate peer's `interests` *if that peer is currently in `peer_sessions`*. (They probably aren't yet at bootstrap time; the entry replays on add_peer regardless via `handle_local_store_event`.)
- `Active { filter, provider != me }` → ignore.
- `Withdrawn` → ignore (only meaningful in motion).

`my_subs` and `broadcast_intents` are NOT rehydrated from disk in Phase 2. Subsystems that called `subscribe` or `subscribe_via` need to re-call on startup. (The existing relay does this — see `relay.rs:460,508`, which call `publish_subscription` in `Relay::run`.) Phase 3+ can rehydrate if anyone actually wants survive-restart-without-rebind.

## Migration of existing call sites

The following call sites change. All are mechanical renames; semantics preserved.

| File | Today | After |
|---|---|---|
| `crates/sunset-relay/src/relay.rs:460` | `publish_subscription(self.subscription_filter.clone(), Duration::from_secs(3600))` | `subscribe(self.subscription_filter.clone(), SubscriptionPolicy::relay_broad())` |
| `crates/sunset-relay/src/relay.rs:508` | same | same |
| `crates/sunset-core/src/bus.rs:127` | `publish_subscription(filter.clone(), Duration::from_secs(3600))` | `subscribe(filter.clone(), SubscriptionPolicy::store_data())` |
| `crates/sunset-core/src/peer/mod.rs:103` | `publish_subscription(filter, Duration::from_secs(3600))` | `subscribe(filter, SubscriptionPolicy::store_data())` |
| `crates/sunset-core/src/peer/mod.rs:128` | `publish_subscription(f, SUBSCRIPTION_TTL)` | `subscribe(f, SubscriptionPolicy::store_data())` |

The `Duration::from_secs(3600)` TTLs were the entry's expiry — under the new model, refresh keeps entries alive on their own cadence, so the caller no longer specifies TTL. The relay's `freshness_threshold: 30s` is a deliberate slow-refresh tuning (the relay maintains many subscriptions; refreshing every 15s for hundreds of clients is fine but not punishing). The other callers use `store_data()` (5s threshold).

Tests have ~8 similar call sites to update — same mechanical pattern. Existing `subscribe_backfill.rs` and `two_peer_sync.rs` integration tests should otherwise continue to pass.

## Testing

Unit tests on `Routes` (in `routing/routes.rs` `#[cfg(test)]`):
- `due_for_refresh` returns only entries past `freshness_threshold / 2`.
- `my_subs` insertion/removal behaves under refresh and unsubscribe.
- `broadcast_intents` insertion/removal is idempotent.

Unit tests on `forward_targets` (free function tests):
- Returns each matching peer once (set semantics).
- Empty `peer_sessions` returns empty set.
- Multi-interest peer matches on any one interest.

Integration tests (new `crates/sunset-sync/tests/phase2_subscribe.rs`, follows `two_peer_sync.rs` shape):
1. **`subscribe` end-to-end.** Receiver calls `subscribe(filter, policy)`; provider writes matching entry; receiver sees it. Verifies the broadcast intent → subscribe_via cascade.
2. **`subscribe_via` end-to-end.** Receiver calls `subscribe_via(filter, provider, policy)` against a specific provider; provider writes; receiver sees. Verifies the lower-layer mechanism.
3. **Auto-resubscribe on peer connect.** Receiver has a broadcast intent; a new peer connects; receiver's `subscribe_via` fires for the new peer; new peer forwards matching entries.
4. **`unsubscribe` stops forwarding.** Receiver subscribes, sees Y, unsubscribes; provider writes Z; receiver does NOT see Z.
5. **Ephemeral coexistence.** Provider publishes an ephemeral datagram matching the receiver's filter; receiver's application callback fires (proves `publish_ephemeral` uses the unified `forward_targets`).
6. **Peer drop drops interests.** Peer A is connected and has named me as provider; A disconnects; my `peer_sessions.remove(&A)` runs; verify `interests` for A are gone in one step.

## What stays the same

- Wire format and behavior of every existing entry type except `SUBSCRIBE_NAME` (which retires).
- Self-author broadcast: self-authored entries still go to every directly connected peer regardless of filter.
- `DigestRequest`/`DigestExchange`/diff/push pipeline.
- Phase 1 substrate (`SubscriptionEntry`, `LinkState`, `Neighbor`, `ProviderTick`, `SubscriptionPolicy`, `covers`, `subscription_name`, reserved-name constants). `LinkState`, `Neighbor`, `ProviderTick`, and `covers` remain unused dead code waiting for Phase 3+.
- Store contract, transport stack, crypto, voice frame format.

## What's new in code

- `crates/sunset-sync/src/routing/routes.rs` (new, ~120 LOC + tests) — `Routes`, `OutboundKey`, `Outbound`, `BroadcastIntent`, `due_for_refresh`.
- `crates/sunset-sync/src/routing/forward.rs` (new, ~40 LOC + tests) — `forward_targets` free function.
- `crates/sunset-sync/src/routing/mod.rs` — re-export the new types.
- `crates/sunset-sync/src/engine.rs`:
  - Rename `PeerOutbound` → `PeerSession`, add `interests` field. Rename `peer_outbound` → `peer_sessions` throughout (~30 callsites, mechanical).
  - Replace `pub registry: SubscriptionRegistry` and `own_filters` with `pub routes: routing::Routes` in `EngineState`.
  - Delete `publish_subscription` / `do_publish_subscription`. Add four methods: `subscribe`, `unsubscribe`, `subscribe_via`, `unsubscribe_via`.
  - Replace the SUBSCRIBE_NAME branch in `handle_local_store_event` with the SUBSCRIBE_PREFIX branch above.
  - Replace 2 callsites of `state.registry.peers_matching` with `forward_targets(&state.peer_sessions, ...)`.
  - Rename `bootstrap_registry` → `bootstrap_routes`; scan SUBSCRIBE_PREFIX only.
  - Add the routing-tick timer and `republish_subscription` helper.
  - Add the auto-resubscriber call in the `AddPeer` path.
- `crates/sunset-sync/src/subscription_registry.rs` — **deleted**.
- `crates/sunset-sync/src/reserved.rs` — drop `SUBSCRIBE_NAME`. Keep `is_reserved` as-is (the `_sunset-sync/` prefix check still applies to the new namespace).
- `crates/sunset-sync/src/lib.rs` — drop `pub mod subscription_registry`.
- 5 production call site migrations (table above).
- ~8 test call site migrations (same pattern).
- `crates/sunset-sync/tests/phase2_subscribe.rs` (new) — six integration scenarios.

## Open questions (deferred, not blocking)

- **Refresh-storm on relay-scale broadcasts.** A relay with N connected clients, each broadcast-subscribed to one filter, has `O(N)` outbound entries refreshing on the routing tick. At N=500, 500 entries × 2s refresh = 250 entries/sec, each a signed insert. Bounded but worth measuring before Phase 5 (which adds voice and probably runs `subscribe_via` at a faster cadence). A simple Phase 3+ optimization: collapse "I want F from every connected peer" into a single signed entry-with-cardinality, served as N peer-pair forwards by the receiver's engine. Not needed for Phase 2.
- **Persistence of `my_subs` and `broadcast_intents` across engine restart.** Phase 2 requires subsystems to re-call `subscribe`/`subscribe_via` on startup. If a real subsystem demands automatic rehydration, Phase 3+ can scan `_sunset-sync/subscribe/*` entries authored by self and rebuild.
- **Exact TTL for Withdrawn entries.** Implementation uses the active entry's original TTL value. Tracking the previous entry's `expires_at` precisely is a Phase 3 nicety.
- **Adaptive routing-tick cadence.** Fixed 500ms. Voice integration (Phase 5) needs sub-200ms; tick becomes "fire at next-due timestamp."
- **De-duplicating DigestRequest** when a receiver subscribed via multiple intents that overlap.
