# Cooperative Relay Design

**Date:** 2026-05-23
**Scope:** A receiver-driven, set-invariant routing layer on top of `sunset-sync` that lets any peer act as a relay for any other peer in the same room — for both the reliable CRDT store and unreliable streams (voice). Dedicated `sunset-relay` machines remain the typical happy-path provider when present, but the system makes no architectural distinction between them and other peers, and continues to operate when they go away. One ranking function (expected time to first data) chooses providers from data the receiver already replicates; one set invariant (`maintain N healthy providers per subscription`) drives failover without thresholds or hysteresis.
**Out of scope (explicit):** Changes to the store contract, transport stack, crypto, or wire format of application data. Onion-style metadata privacy (peers in a room already know each other). DHT-based discovery. New per-data-type encodings. LAN/mDNS bootstrap. Provider incentives or economics. Adversarial relays (existing semi-trusted relay threat model carries over).

## Goal

Today, sunset's data plane is fully-connected P2P plus relay-mediated for everyone else. A peer either has a direct WebRTC connection to another peer or talks to it through a configured `sunset-relay`. Voice is strictly mesh. Two consequences:

1. **Relay outage is felt as a UX outage.** If the configured relay dies during a session and two peers can't establish direct WebRTC, the conversation stops, even though every peer involved is online and every peer can talk to *some* other peer in the room.
2. **Bandwidth is asymmetric.** A peer with a great link to the relay shoulders little load; a peer with a marginal link to the relay forces all its traffic through the relay even when a neighbor would have a faster path.

The user-visible goal: a room continues to work as long as the room's peers form a connected component, with no perceptible degradation when the relay is reachable and graceful operation when it isn't. Voice failover is gap-free; reliable-data failover is propagation-bounded (a few seconds).

The architectural goal: any peer can fulfill a subscription for any other peer. Routing decisions are local to each receiver, computed from data that already gets replicated (subscription entries) plus two new tiny gossip channels (link-state, provider-tick). No central coordinator, no topology consensus, no global plan. The current `sunset-relay` becomes "a peer that happens to be always-on with broad subscriptions" — its preferential treatment falls out of the ranking function rather than being baked into the protocol.

## Non-goals (and why)

- **Changes to the store contract.** The new behavior is expressed entirely in subscription entries, link-state entries, and provider-tick entries living in reserved `_sunset-sync/` namespaces. The store's LWW + TTL + signature semantics carry every piece of new state.
- **Multi-source dedup beyond what the data plane already does.** CRDT idempotency handles store entries; a small forwarder-side LRU on `(source, seq)` handles voice frames. We do not introduce a routing-layer dedup table that mirrors data the receiver can compute.
- **Adversarial peers.** A misbehaving peer can drop subscriptions targeted at it, lie in its link-state, or refuse to forward. The room remains live as long as one honest path exists, but we do not attempt to detect or punish dishonesty. Same trust posture as the existing semi-trusted relay.
- **Onion routing / metadata privacy.** Room members already know each other. Forwarder knowledge of "A talks to C" is not a new disclosure.
- **A coordinator-elected mode.** Discussed and rejected — a coordinator adds failure modes (election lag, plan staleness, single-decider error) the receiver-driven model does not have.

## Architecture

```
Application (chat, voice)
        │
        ▼
sunset-sync engine ─── peer_outbound (direct neighbors, transport)
        │
        ▼
NEW: Routing layer
   ┌──────────────────────────────────────────────────────────────┐
   │ Subscription state (receiver-side):                          │
   │   For each filter f the application wants:                   │
   │     ProviderSet { target_n, freshness_threshold,             │
   │                   subscribed: Map<PeerId, LastSeen> }        │
   │   Invariant: |healthy(subscribed)| >= target_n               │
   │                                                              │
   │ Provider state (provider-side):                              │
   │   For each (filter, requester) currently named in my inbox:  │
   │     forward matching items; recursively subscribe upstream   │
   │     if I don't already cover the filter.                     │
   └──────────────────────────────────────────────────────────────┘
        │
        ▼
Reserved namespace (just data, replicated by the existing engine):
   _sunset-sync/subscribe/<filter-hash>/<provider-id>   (per pair, Active|Withdrawn)
   _sunset-sync/links                                   (one per peer)
   _sunset-sync/provider-tick                           (one per peer, monotonic)
```

The routing layer is a policy on top of the engine. The engine still moves bytes; the routing layer decides who-to-whom. Every "decision" is either published as a signed entry (subscription, withdrawal) or computed at the receiver from entries it already replicates (candidate ranking).

## Wire types

Three new entry shapes, all in `_sunset-sync/` reserved namespace, all signed by their publisher, all subject to existing LWW/TTL semantics.

### Subscription entry

```rust
// stored at (publisher = receiver, name = "_sunset-sync/subscribe/<filter-hash>/<provider-id>")
pub enum SubscriptionEntry {
    Active { filter: Filter, provider: PeerId },
    Withdrawn,
}
```

- `<filter-hash>` is `blake3(postcard(filter))_hex`; `<provider-id>` is the provider's hex peer id. Together they make the name unique per (filter, provider) pair from a given receiver, so refresh is idempotent (`LWW` collapses re-publishes onto the same key) and distinct pairs coexist as distinct entries.
- `Active { filter, provider }` redundantly carries `filter` and `provider` for self-verification (the values must match the key). The provider reads it to know what to forward.
- `Withdrawn` is published at the same key when the receiver wants to stop the subscription faster than its TTL would otherwise allow. It is published with `expires_at` ≥ the previous entry's, so it propagates through the network normally and reaches multi-hop peers before being garbage-collected. The provider stops forwarding on the `Replaced` event.

Cadence: receivers refresh `Active` entries periodically (e.g., every `freshness_threshold / 2`) by re-publishing with extended TTL. Lazy withdraw = stop refreshing. Active withdraw = publish `Withdrawn` at the same key.

### Link-state entry

```rust
// stored at (publisher = self, name = "_sunset-sync/links")
pub struct LinkState {
    neighbors: Vec<Neighbor>,
}
pub struct Neighbor {
    peer: PeerId,
    rtt_ms: u16,
    last_success_ts: u64,
}
```

The publisher lists peers they are directly connected to right now, with the publisher's own heartbeat-measured RTT and the timestamp of the last successful exchange. Republished every ~30s with ~90s TTL.

There is no `broad_subscriber` field, no `load_hint`, no anything else. Coverage is read from the publisher's own subscription entries (already replicated). Load is observed empirically through tick latency. Adding either field would denormalize data the receiver can already compute.

### Provider-tick entry

```rust
// stored at (publisher = self, name = "_sunset-sync/provider-tick")
pub struct ProviderTick {
    seq: u64,
}
```

A monotonically-increasing sequence number, republished every ~2s with ~10s TTL. Receivers subscribed to a provider see the provider's ticks arrive at their own subscription path; the arrival rate and freshness is the liveness signal for *that provider via that path*.

For voice subscriptions, the voice frames themselves serve the same role — same mental model ("I expect something from this provider regularly"), no need for a separate beacon during active flow. Provider-tick covers idle / sparse subscriptions where data would not otherwise arrive.

## Receiver behavior

Per active filter the application wants:

```rust
pub struct SubscriptionPolicy {
    target_n: usize,                  // 1 = reactive, 2 = dual-delivery
    freshness_threshold: Duration,    // how long to wait before calling a provider dead
}

// receiver state, per filter
pub struct ProviderSet {
    policy: SubscriptionPolicy,
    subscribed: HashMap<PeerId, LastSeen>,
}
```

The receiver maintains one invariant: **at least `target_n` providers in `subscribed` are healthy** (`now - last_seen <= freshness_threshold`). Two actions, both driven directly by the invariant:

1. When healthy count drops below `target_n`, pick the top-ranked unsubscribed candidate and publish an `Active` subscription naming them. Add them to `subscribed`.
2. When a subscribed provider's `last_seen` falls more than `freshness_threshold` behind, publish a `Withdrawn` at their subscription key and drop them from `subscribed`.

That is the whole receiver loop. There is no switch decision, no comparison of A vs B, no hysteresis, no minimum dwell time. A flapping provider is replaced on its first failure; if it recovers we have `target_n + 1` healthy providers temporarily, which is fine and absorbs the next failure.

### Candidate ranking

The "top-ranked unsubscribed candidate" is the one with the smallest `expected_first_data(candidate, filter)`:

```
expected_first_data(c, f) =
    if I have observed c delivering recently:
        the observed delivery latency (e.g., median of recent tick latencies)
    else if c's published subscriptions cover f:
        my RTT estimate to c
    else:
        my RTT estimate to c + COLD_START_BUDGET
```

One function. The three branches inside it are computing the right number from whatever data is available — observation when we have it, coverage when we don't, conservative estimate when we have neither. They are not policy variants.

Coverage is `exists s in c.subscriptions: s ⊇ f`, computed from subscription entries the receiver already replicates. RTT is from the receiver's own heartbeat (for direct peers) or summed from link-state along the shortest known path (for indirect). Load is captured implicitly: an overloaded provider's ticks arrive slowly, raising its observed latency, lowering its rank. No self-reported load field exists.

`COLD_START_BUDGET` is a single constant approximating "publish subscription + recursive resolution + first delivery." A few hundred milliseconds is a reasonable default; the exact value is not load-bearing because once observation begins, observed latency replaces the estimate.

## Provider behavior

```rust
// when an entry at (any_peer, "_sunset-sync/subscribe/.../<my-id>") is Inserted or Replaced as Active { filter, provider == self }:
//   start forwarding store events matching filter to the requester
//   if I don't already cover filter, publish my own Active subscription for filter
//   targeting my best unsubscribed candidate (the same ranking function the receiver uses).

// when the same entry is Replaced as Withdrawn, or Expired:
//   stop forwarding to that requester
//   if no remaining requesters use the upstream subscription I added, withdraw it too.
```

That is the entire provider loop. Multi-hop emerges from the recursion — at each hop the same receiver-side ranking picks the next hop. No path vector is needed; data-layer dedup keeps loops harmless (CRDT idempotency for store entries, forwarder-side LRU on `(source, seq)` for voice frames). Loops are correctness-safe; the worst case is one wasted hop before dedup terminates them.

## Liveness

There is one freshness clock per `(filter, provider)` pair the receiver is subscribed to. The clock is updated by any of:

- An item matching `filter` arriving via that provider.
- A `_sunset-sync/provider-tick` from that provider arriving via that provider's path.

For the second bullet to work, the receiver needs the provider's ticks to flow to it via that provider. The routing layer handles this implicitly: when the receiver first subscribes to filter `F` via provider `P`, it also ensures an `Active { _sunset-sync/provider-tick, P }` subscription exists (one such subscription per provider, shared by every filter the receiver requests from `P`). The tick subscription is withdrawn when no filter via `P` remains.

The transport-layer heartbeat (existing) continues to drive `PeerSupervisor` reconnection logic — it is *connection management*, not routing. A transport disconnect on a directly-connected provider is a free, fast signal that the routing layer treats as "freshness_threshold has elapsed instantly" for every subscription pinned to that provider.

## Policy surface

```rust
pub struct SubscriptionPolicy {
    pub target_n: usize,
    pub freshness_threshold: Duration,
}
```

Two knobs. The voice subsystem sets `target_n = 2` while a call is active, `1` otherwise; everything else defaults to `target_n = 1`. Sensible defaults for `freshness_threshold`: ~200 ms for voice, ~5 s for store data. Both surface as user-toggleable preferences (per the design discussion: dual delivery should be configurable, default-on for voice).

There is no other policy. There are no per-provider weights, no scoring overrides, no failover thresholds, no dwell times. Adding any of those would re-introduce the enumerated-cases-as-algorithm anti-pattern that the design explicitly avoids.

## Bootstrap

A first-time peer joins a room as today:

1. Dial the configured relay (existing flow).
2. Once connected, subscribe to `_sunset-sync/links` and `_sunset-sync/provider-tick` in the room's keyspace (via the relay, since at this moment it is the only candidate with proven low RTT and known coverage).
3. Within a couple of seconds, the link-state replication populates the candidate set with every other peer the relay has heard from.
4. The receiver's normal slot-filling loop runs; the relay typically continues to win the first slot (low RTT, broad coverage) while better-positioned direct peers may take subsequent slots (`target_n = 2` cases).

No new bootstrap protocol exists. If no relay is configured (or it's down) and the peer has another peer's contact information (e.g., shareable link), the same flow runs with that peer as the seed.

## Failure scenarios

| Scenario | Behavior |
|---|---|
| Relay goes down mid-session | Each receiver's freshness clock for the relay expires within `freshness_threshold`; the slot-filling loop publishes subscriptions to the next-best candidate. For `target_n = 2` voice, the second slot was already serving; gap = 0. For `target_n = 1` store, gap ≈ `freshness_threshold + propagation`. |
| Direct peer-to-peer link degrades | Same mechanism; the affected provider's tick latency rises, its rank falls, on the next slot replenishment it loses its position. |
| Two peers temporarily can't reach each other but both reach a third | The third becomes a viable provider via the standard ranking. Recursive subscription resolves automatically. |
| Loop in subscription chain | Reliable: second arrival is `Error::Stale` at the store, not re-forwarded. Voice: forwarder LRU on `(source, seq)` drops the duplicate. One wasted hop, then natural termination. |
| Receiver picks a candidate that doesn't have the data | Candidate publishes its own subscription upstream using the same ranking. First data arrives after one cold-start round-trip; subsequent items flow at steady-state latency. |

## What stays the same

- Store contract (LWW, TTL, signature verification, content-addressed blobs).
- Transport stack and Noise framing.
- Voice frame format (`SignedDatagram` with AEAD-bound sender identity) — already forward-friendly; no changes needed.
- `sunset-relay`'s configuration, deployment, and trust model. It picks up the new behavior by virtue of running the same engine; the receiver-driven ranking is what makes it preferred when it's reachable.
- Existing `SubscriptionRegistry` and subscribe-triggered backfill — the routing layer publishes through them.

## What's new in code (high level — implementation plan will detail)

- `crates/sunset-sync/src/routing/` (new module): `ProviderSet`, the slot-filling loop, the ranking function, link-state and provider-tick publishers/consumers.
- New filter variants in the reserved namespace are just `NamePrefix` patterns; no `Filter` enum change required.
- `SubscriptionEntry` postcard type (replaces today's bare `Filter` payload at the existing subscribe path; existing single-slot subscriptions become the `target_n = 1` case with the relay as default provider).
- Receiver and provider tasks integrate with the existing per-peer outbound machinery.

## Compatibility / migration

The wire format of subscription entries changes (from bare `Filter` to `SubscriptionEntry` enum, and the entry name gains the `<filter-hash>/<provider-id>` suffix). This is a breaking change at the sunset-sync layer. Migration path:

- Pre-1.0, no compatibility shim. The codebase ships the new shape directly.
- `sunset-sync`'s own tests update to use the new `SubscriptionEntry` postcard payload; the store conformance suite is unaffected (it tests LWW / TTL / signature verification at the byte level and is agnostic to the payload shape).
- Existing `_sunset-sync/subscribe` entries in any local store fail to deserialize under the new sync layer; they are logged and skipped on insert and removed when they expire by TTL. Receivers publish replacement entries in the new shape on first run.

## Open questions (deferred, not blocking)

- **Tuning `COLD_START_BUDGET` and freshness defaults.** Initial values are educated guesses; real traffic measurement will inform adjustments. Not architectural.
- **mDNS / LAN discovery as an additional seed source.** Orthogonal; slots into the same candidate ranking when added.
- **Tree-shaped overlays for very large rooms (100+ listeners on one source).** The receiver-driven model handles this correctly but not optimally; a future optimization could express tree edges in the same subscription vocabulary. Defer until a real workload requires it.
- **Per-application customization of the ranking function.** Today it's one function; if a use case demands a different ordering (e.g., privacy-preferred routing), it can be parameterized then. Not now.
