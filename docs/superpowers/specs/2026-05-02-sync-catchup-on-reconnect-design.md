# Sync catch-up on reconnect

## Problem

When a sunset client reconnects to a relay (network blip, page resumed
from background, supervisor redial after relay restart), it does not
catch up on chat events that arrived at the relay while the client was
disconnected. The relay holds the data; the client never asks for it.

The symptom is user-visible: a tab that was offline for a few seconds
returns to find an empty chat where the network bystander saw fresh
messages.

## Root cause

The engine fires Bloom digests in three places today, and reconnects
hit none of them in the right way:

- `handle_inbound_event::PeerHello` (post-Hello bootstrap) calls
  `send_bootstrap_digest(peer)` once per connection.
- `tick_anti_entropy` (every `config.anti_entropy_interval`, default
  30 s) builds a digest over `bootstrap_filter` for every connected
  peer.
- `do_publish_subscription` (added in PR #10, merged 2026-05-02)
  fires `send_filter_digest(peer, filter)` to every connected peer
  immediately after a successful publish — closing the late-subscriber
  gap where data already at a peer wouldn't reach us until the next
  anti-entropy tick that touched the right namespace.

The first two build their Bloom over `self.config.bootstrap_filter`,
which the default `SyncConfig` hardcodes to
`Filter::Namespace(SUBSCRIBE_NAME)`. That covers the subscribe-namespace
only. Chat (`<room_fp>/msg/...`), WebRTC signaling
(`<room_fp>/webrtc/...`), and any other room-namespace entry is never
asked about by these two paths — even though our peer (the relay)
holds them and would happily ship them on a `DigestExchange` reply.

PR #10's per-publish digest closes the gap *at publish time*, but
`publish_room_subscription` is only called from the Gleam app's
`RelayConnectResult(Ok(_))` handler, which fires off the *initial*
`add_relay`. The supervisor's redials never re-trigger that handler
— they emit `PeerAdded` events only — so the per-publish digest only
ever fires once, at page bootstrap. After a reconnect, no path asks
"give me what I missed under filter X."

The push side keeps things partially in sync: when the user calls
`publish_subscription`, the new entry lands in the local store, the
local subscription stream fires it, and `handle_local_store_event`
broadcasts it to every connected peer (self-authored entries bypass
the registry filter). So the relay *learns* the filter. But that only
causes the relay to push *future* matching entries — it doesn't pull
the historical ones we missed while disconnected.

The frontend is not the wrong place to fix this: catch-up is a
sync-layer responsibility. Frontend hosts (Gleam, future TUI, future
mod) should be able to call `add_relay` and `publish_subscription`
once and trust the engine to keep the local store consistent across
disconnects.

## Design

Close the gap inside the engine. Two pieces, plus a shared helper.

The generalized digest helper `send_filter_digest(to, filter)` already
exists on master (PR #10), and `send_bootstrap_digest` is already a
one-liner that delegates to it. We reuse those as-is — no refactor.
`tick_anti_entropy` still inlines its own `build_digest` call instead
of using the helper; we'll route it through the helper as part of
Change 2 so the new per-filter fires share the same code path.

### Change 1 — fire per-filter digests on `PeerAdded`

In `handle_inbound_event::PeerHello`, after `send_bootstrap_digest(peer)`,
walk our own published subscriptions and fire one `send_filter_digest`
per filter to the freshly-connected peer.

"Our own published subscriptions" = entries in the local store with
`name == SUBSCRIBE_NAME` and `verifying_key == self.local_peer.0`.
Iterated on demand via `store.iter(Filter::Namespace(SUBSCRIBE_NAME))`
and filtered to self-authored entries; the parsed `Filter` from the
entry's content block is what we send the digest over.

This is the targeted reconnect fix. Every redial — supervisor backoff,
relay restart, network blip recovery, browser tab resume — runs the
same `PeerHello` handler in the engine, so the catch-up is automatic
without any host-level coordination.

### Change 2 — extend `tick_anti_entropy` to cover our own filters

In `tick_anti_entropy`, after the existing `bootstrap_filter` digest,
walk our own published subscriptions (same iteration as Change 1) and
fire `send_filter_digest` for each, to every connected peer. Periodic
catch-up belt-and-suspenders for cases where a connection stays alive
but data was nevertheless missed (transient relay state bug, future
race, anything that punctures push routing).

Same iteration, same firing primitive, same wire message — just on the
periodic timer rather than the per-connection edge.

### Common helper

Both changes need to enumerate our own published filters. Factor that
into one private async method on `SyncEngine`:

```rust
/// Walk `SUBSCRIBE_NAME` entries authored by `self.local_peer` and
/// return the parsed filters. Iterates the store on each call —
/// callers (PeerHello, anti-entropy) already do non-trivial async
/// work, and the typical client has 0–2 self-authored subscribe
/// entries, so the cost is negligible.
async fn own_published_filters(&self) -> Vec<Filter>;
```

Returning `Vec<Filter>` keeps the call sites simple — they just
iterate and fire.

## Data flow on reconnect (post-fix)

```
1. supervisor.fire_due_backoffs → engine.add_peer(addr).await
2. engine spawns dial task → spawn_run_peer → Hello round-trip
3. peer.rs sends InboundEvent::PeerHello(... registered: ack ...)
4. engine.handle_inbound_event::PeerHello:
     - inserts peer_outbound, peer_kinds
     - emits EngineEvent::PeerAdded
     - fires registered ack (PR #5: add_peer.await wakes here)
     - send_bootstrap_digest(peer)               -- existing
     - for filter in own_published_filters():    -- NEW
         send_filter_digest(peer, &filter)
5. peer responds to each digest with EventDelivery containing entries
   we're missing.
6. handle_event_delivery inserts each entry → local_sub fires →
   chat UI sees the new messages via the existing subscription path.
```

The new step 4 ("NEW") is the entire user-visible behavior change.
Same wire format. Same response handler.

## Bandwidth and cost

- Bloom is bounded by `config.bloom_size_bits`. One bloom per filter.
- Typical client has one self-authored subscribe entry (the room
  filter). So Change 1 adds one `DigestExchange` per reconnect.
- Change 2 adds N digests (where N is the number of self-authored
  subscribe entries) per peer per `anti_entropy_interval`. Default
  interval is 30 s; default N is 1; default peer count is 1. So
  baseline overhead is one extra digest every 30 s, for clients that
  could otherwise be missing data.
- Relay-side scan in `handle_digest_exchange` is bounded by
  `Filter::matches`. For chat namespace prefixes that's filesystem
  scan over chat history; cost scales with relay store size, but not
  with how many filters the client published.

## Error handling

`store.iter` failures: log + return empty. Same posture as the
existing `replay_existing_subscriptions` and `send_bootstrap_digest`
helpers — a single transient store error during startup or anti-
entropy must not tear down the engine.

`parse_subscription_entry` failures: skip that entry. The entry's
filter is corrupt; the digest helper would have nothing meaningful
to send anyway.

`send_filter_digest` already swallows the per-peer outbound send
errors (returns silently when `peer_outbound.get(to)` is None — peer
disconnected mid-fire). No new error paths.

## Testing

Two unit tests in `crates/sunset-sync/src/engine.rs`:

1. **`peer_hello_fires_filter_digest_for_self_published_subscriptions`**
   - Pre-populate the engine's store with one self-authored
     `SUBSCRIBE_NAME` entry whose value-block encodes a chat-like
     `Filter::NamePrefix(b"room/")`.
   - Drive a `PeerHello` event for a connected peer (with a captured
     outbound channel, like `publish_subscription_sends_filter_digest_to_connected_peers`).
   - Drain the channel; assert at least one `DigestExchange` whose
     `filter` matches the published filter (in addition to the
     existing `SUBSCRIBE_NAME` digest).

2. **`anti_entropy_tick_fires_filter_digest_for_self_published_subscriptions`**
   - Same store seed.
   - Stand up one connected peer (no PeerHello side-effects).
   - Call `engine.tick_anti_entropy()` directly.
   - Drain; assert one `DigestExchange` over the published filter,
     plus the existing one over `bootstrap_filter`.

E2E coverage: `relay_restart.spec.js` already exercises the
"reconnect after server restart" path. With Change 1, post-restart
chat traffic that arrived at the relay during the gap is delivered
to the client without further user action. The existing post-restart
assertions remain the user-visible contract.

## Out of scope

- **Frontend changes.** Both changes are sync-layer-only. Gleam's
  `add_relay` / `publish_room_subscription` flow stays exactly as it is.
  Future hosts (TUI, mod) inherit the catch-up automatically.
- **Filter-aware bootstrap protocol.** `bootstrap_filter` stays hardcoded
  to `SUBSCRIBE_NAME`; we don't change `SyncConfig`. Per-publish-filter
  digests are an *additional* exchange, not a replacement for the
  bootstrap one.
- **Server-pushed catch-up.** A natural alternative is to have the
  relay push matching entries when it sees a new subscription register
  in its registry, rather than waiting for the client to ask. We use
  the bloom-digest approach instead, for bandwidth efficiency at scale
  (the relay never sends entries the client already has) and for
  symmetry with how `bootstrap_filter` already works.
- **Persistent client storage.** When `sunset-store-indexeddb` (browser
  persistence) lands, the client's bloom on reconnect will be non-empty,
  and Change 1's bandwidth advantage over a naive replay grows. Until
  then, the bloom is empty on every fresh page load and the digest is
  effectively a "send everything" request.
