# Subscribe-triggered backfill — design

- **Date:** 2026-05-03
- **Status:** Approved
- **Scope:** Close the engine-side race in `sunset-sync` where third-party-authored entries that arrive *before* a recipient's `SUBSCRIBE_NAME` entry has been parsed into the registry are stored locally but never forwarded. Adds one new forwarding trigger: registry-update itself.
- **Parent:** [`2026-04-25-sunset-store-and-sync-design.md`](2026-04-25-sunset-store-and-sync-design.md), [`2026-04-27-sunset-relay-design.md`](2026-04-27-sunset-relay-design.md).
- **Motivation source:** Follow-up acknowledged in commit `c27ad46e` ("e2e: voice tests wait for member-list visibility before connect_direct"). The C2b voice work shipped a test-side workaround for this race; the engine-side fix lives here.

## Background — what is missed today

`sunset-sync::engine::handle_local_store_event` (engine.rs:843–896) is the single forwarding path. For each incoming entry it routes by authorship:

- **Self-authored entries** broadcast to all connected peers, registry-agnostic (engine.rs:882–885).
- **Third-party entries** forward only to peers whose registered filter matches, via `SubscriptionRegistry::peers_matching` (engine.rs:886–895).

The race: if a third-party entry `E` arrives in the local store *before* the recipient `B`'s `SUBSCRIBE_NAME` entry has been parsed into the registry, `peers_matching` returns ∅ for `B`, and `E` is stored but never pushed. When `B`'s `SUBSCRIBE_NAME` arrives later, the registry is updated — but `handle_local_store_event` only fires on *new* writes, so `E` sits in the store without a forwarding trigger. `B` eventually closes the gap via the next anti-entropy tick (PR #13 — periodic, on the order of tens of seconds) or a reconnect (`PeerHello` fan-out).

Latency-sensitive flows — WebRTC SDP offers, presence churn at room join — break inside that window. The C2b symptom: Alice's offer published 50ms before the relay parses Bob's `SUBSCRIBE_NAME` → Bob's `connectDirect` hangs until anti-entropy fires → 30s test timeout, "call won't connect" in production.

The fix makes registry-update itself a forwarding trigger.

## Scope

This is an engine-level change in `sunset-sync`. The same code path runs at every `SyncEngine` instance — relay, native client, browser client, federated peer — so the fix benefits every deployment, not just the relay. The branch name `relay-backfill-on-subscribe` reflects the immediate motivation; the actual change is engine-wide.

## Design

### Trigger and mechanism

In `handle_local_store_event`, after `parse_subscription_entry` succeeds and the registry is updated:

1. Determine whether the registry insert was a *new* peer or a *changed* filter for an existing peer. If the filter is unchanged (TTL refresh of the same filter), no backfill is triggered.
2. Skip if `peer_vk == self.local_peer` (a self-published `SUBSCRIBE_NAME`, e.g., the relay's broad subscription at startup).
3. Skip if the peer is not currently connected (no transport to deliver over; registry insert still happens, and a future `PeerHello` will fire `fan_out_digests_to_peer`).
4. Otherwise, walk the local store for entries matching `new_filter` and push them to the peer as `EventDelivery` messages.

The mechanism is a direct push, not a `DigestExchange`. `DigestExchange` as currently defined carries the *sender's* bloom and prompts the *receiver* to push back what the receiver has that the sender doesn't (see `handle_digest_exchange` at engine.rs:715–749). That direction works for `publish_subscription`'s catch-up — the new subscriber sends the digest with an empty bloom and the relay responds with everything matching — but it's the wrong direction for backfill, where the local engine already holds entries the peer is missing. A direct `EventDelivery` walk is the symmetric counterpart of what `handle_local_store_event`'s push branch does on a fresh write, just applied to already-stored entries.

The walk uses the existing `Store::iter(filter)` API — the same primitive `build_digest` and `entries_missing_from_remote` already use. No new message variants, no new wire types — one new private helper on `SyncEngine` (the push loop) and one new call site in `handle_local_store_event`.

### Registry change-detection

`SubscriptionRegistry::insert(vk, filter)` is updated to return whether the filter is new, changed, or unchanged. Concretely it returns an `Option<Filter>` of the previous filter (`None` for new peer, `Some(old)` for existing). The caller compares `old` to `new` and triggers backfill iff they differ.

This is a small refactor of one method, not an abstraction. No new traits, no callbacks. The change-detection lives at the call site that already does the registry update.

### Concurrency

Registry update and digest dispatch happen on the same task as existing event handling. `send_filter_digest` is already called from sibling event handlers (PR #13's `PeerHello` fan-out, anti-entropy tick); this design adds one more call site inside the same engine event loop, not new concurrency.

## Edge cases

- **Filter refresh, no change** — peers republish `SUBSCRIBE_NAME` periodically for TTL refresh. Skip backfill in this case. Anti-entropy already covers steady-state.
- **Filter changed** — peer rotated rooms or widened interest. Fire one digest over the new filter. Old entries that no longer match the new filter aren't unpushed (already delivered, still in peer's store; LWW + GC handle cleanup as designed).
- **Self-published `SUBSCRIBE_NAME`** — skip. Backfilling to self is meaningless.
- **Peer not connected** — registry insert still happens (so future live-forwarding works); digest is skipped. `PeerHello` covers them on connect.
- **Federation / multi-relay** — relay-to-relay forwarding goes through the same `handle_local_store_event`, so federation benefits transparently. No separate code path.
- **Initial startup** — `replay_existing_subscriptions` runs before the live listener and before peers connect, so there is no race at startup. Backfill is purely for the live registry-update path.

## Tests

### New regression test

Added alongside `crates/sunset-sync/tests/two_peer_sync.rs`. Drives the race deterministically:

1. Two peers `A`, `B` connected to a forwarder (or directly — same code path).
2. `A` publishes entry `E` *before* `B` publishes `SUBSCRIBE_NAME`.
3. Block until `E` has been observed at the forwarder's store (deterministic via store cursor — not a sleep).
4. `B` then publishes `SUBSCRIBE_NAME` for a filter matching `E`.
5. Assert `B` receives `E` within a short bound — without polling on `knows_peer_subscription` or any other engine-internal state.

Litmus check from CLAUDE.md's debugging discipline: a real API user calling `publish_subscription` and expecting subsequent matching entries to arrive is exactly what the contract should guarantee, regardless of write ordering relative to peers' subscriptions.

### Workaround removals (acceptance criteria)

The strongest evidence that the engine fix actually closes the race is that the test-side workarounds for it can be removed and the affected tests still pass — flake-free — under their original timeouts:

- `crates/sunset-sync/tests/two_peer_sync.rs:91–98` — drop the `knows_peer_subscription()` poll. The test should still pass.
- `web/e2e/voice_network.spec.js` — drop the `waitForFunction(memberVisible(peer))` calls before `connectDirect`. Drop the `on_members_changed` plumbing in `web/voice-e2e-test.html` if it has no other consumer. The voice byte-equal and peer-state tests should pass under the original 30s timeout, locally and on the slow CI runner where the original 20% flake reproduced.

If either workaround can't be removed cleanly, the fix is incomplete — that's the primary signal during implementation.

### Verification matrix

- New regression test passes.
- Existing `two_peer_sync` and `multi_relay` tests still pass after workaround removal.
- E2E `voice_network.spec.js` passes without member-visibility waits, locally and on the slow CI runner.
- `cargo clippy --workspace --all-features --all-targets -- -D warnings` clean.
- `cargo fmt --all --check` clean.

## Out of scope

- **Anti-entropy tick frequency / topology.** Backfill complements anti-entropy at the registry-update edge; tuning anti-entropy itself is unrelated.
- **GC of expired `SUBSCRIBE_NAME` entries.** The registry's response to a peer's subscription expiring is governed by existing TTL pruning; backfill doesn't change it.
- **A formal "ordering proof".** The structural argument is sufficient: the only paths to forwarding are now (a) live writes via `handle_local_store_event`, (b) registry updates, (c) anti-entropy ticks, (d) `PeerHello` fan-out. The first three cover live operation; the fourth covers reconnect. No remaining path is missing a trigger.
- **Performance benchmarks.** Backfill cost on registry update equals one store walk plus one `EventDelivery` message. The cost is bounded by the trust set: only trust-admitted peers can publish `SUBSCRIBE_NAME` entries, and an in-trust peer can already trigger comparable work via `publish_subscription`'s post-publish digest. Filter-rotation adversaries do not introduce a new attack surface.
- **Other `knows_peer_subscription` callers in the workspace.** Removing the poll in `tests/two_peer_sync.rs` is the spec's promised acceptance criterion. Other callers exist in `tests/ephemeral_two_peer.rs`, `crates/sunset-core/tests/`, `crates/sunset-relay/tests/multi_relay.rs`, and `crates/sunset-sync-ws-native/tests/`; auditing them is a follow-up.
- **Ephemerals.** The `knows_peer_subscription` poll in `crates/sunset-sync/tests/ephemeral_two_peer.rs` is **not** a workaround — it is a real precondition. Ephemeral datagrams (`publish_datagram`) are routed through the registry independently of `handle_local_store_event` and have no replay semantics. Backfill, which walks the durable store, cannot rescue them. That test stays as written.
