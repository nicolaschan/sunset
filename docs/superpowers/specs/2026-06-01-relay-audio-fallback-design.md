# Relay audio fallback — design

Status: draft (autonomous, revised after adversarial review). Extends the
cooperative-relay data plane (PR #108) and the voice runtime.

## Intent

Make voice audio **work when two peers cannot form a direct WebRTC link**
(symmetric NAT, blocked UDP, ICE failure) by carrying the audio **through a
relay both peers are already connected to**. Direct WebRTC stays preferred: when
a direct link is available the audio takes it and the relay carries nothing for
that pair; the system keeps retrying WebRTC and switches back the moment it
connects.

Success = a peer pair that *cannot* form a direct WebRTC link still hears each
other (audio provably crosses the relay), and a pair that *can* uses it (the
relay forwards no audio for them), with automatic, bounded switchover both ways.

## Background — current reality (grounded in code; corrections from review)

- Voice frames are **ephemeral `SignedDatagram`s** via `Bus::publish_ephemeral`,
  named `voice/{room_fp}/{sender_pk}`; the encrypted `VoicePacket::Frame` carries
  a monotonic per-sender `seq` (`runtime/mod.rs:181`). Heartbeats
  (`runtime/heartbeat.rs:53`) publish under the **same** ephemeral name and carry
  **no** seq (`packet.rs` `Heartbeat`). **Voice frames + heartbeats are the only
  ephemeral publishers; voice *presence* is durable** (`voice_presence_publisher`
  uses `publish_durable`) and therefore already relays through the store path.
- `engine.publish_ephemeral` fans out via the shared
  `forward_targets(peer_sessions, vk, name)`. **Inbound ephemeral
  (`handle_ephemeral_delivery`) verifies the signature then `dispatch_ephemeral_local`
  only — it never re-forwards.** Durable `EventDelivery` re-forwards transitively
  (`fanout_application_entry`, engine.rs:1326) because it goes through the store:
  insert fires a store event, and the store's LWW/`Stale` makes a node fan out
  **only entries new to its store** — that is the loop-prevention, for free.
  Ephemeral bypasses the store, so it has neither the trigger nor the guard.
- `SignedDatagram { verifying_key, name, payload, signature }`;
  `datagram_signing_payload` covers `(verifying_key, name, payload)`. **There ARE
  frozen wire tests:** `datagram_payload_frozen_vector` pins
  `blake3(datagram_signing_payload)` to a hex vector, and
  `datagram_payload_excludes_signature_field` pins the covered-field set
  (`canonical.rs`). Both change when `seq` enters the payload.
- `peer_sessions: HashMap<PeerId, PeerSession>` is keyed by each connection's
  **per-transport auth identity**: a direct WebRTC link to A is keyed under A's
  PeerId with `kind == Secondary`; the relay link is a *separate* peer (the
  relay's PeerId, `kind == Primary`). `current_peers()` exposes
  `(PeerId, TransportKind)`; `connected_peers()` discards the kind. "No direct
  link to A" = **A absent from `current_peers()` with `kind == Secondary`**.
- Voice currently subscribes to the **broad** prefix `voice/{room_fp}/` via
  `Bus::subscribe` (a `BroadcastIntent` that auto-arms a `subscribe_via` to every
  connected peer). So today every connected peer — relay included — is already
  armed to forward all room voice to the client; relay forwarding is the *only*
  missing piece, not the subscription.
- The supervisor retries `webrtc://A` dials **forever** (`Backoff` has no
  max-attempts); one `IntentId` per peer.

## Design: two independent layers

1. **Data plane — ephemeral re-forward, structurally identical to durable.**
2. **Control plane — voice chooses the *provider* (direct peer vs relay) per
   call participant, derived (not toggled) from direct-link presence.**

They meet at the `subscribe_via(filter, provider)` routing primitive — but the
voice crate cannot reach it today (it holds only `DynBus` + `Dialer`), so the
control plane requires a **named cross-layer seam widening** (Layer-2
§"Control-plane seams"), not "no new concept." The routing *semantics* are
reused; the *access path* is new and specified below.

---

## Layer 1 — Data plane

### Re-forward = the durable path, minus the store

Durable re-forward is: foreign entry → `forward_targets(peer_sessions, vk, name)`
(interest-gated) excluding the source; loop-prevention is "only fan out what was
new to my store." Ephemeral gets the **same** shape; the only thing it lacks is
the store's idempotency, so it carries the **single minimal in-memory analog**:

`EngineState.ephemeral_hwm: HashMap<(VerifyingKey /*sender*/, Bytes /*name*/),
u64 /*highest seq forwarded*/>`.

`handle_ephemeral_delivery(from, datagram)`, after the existing signature check:

```
key = (datagram.verifying_key, datagram.name)
if datagram.seq <= hwm[key]:        // analog of store `Stale`: already passed through
    return                          // (drop — terminates loops)
hwm[key] = datagram.seq
dispatch_ephemeral_local(datagram)                       // local subscribers
forward_targets(peer_sessions, vk, name) \ {from}        // interest-gated re-forward
    .for_each(|peer| peer.tx.send(EphemeralDelivery))    // never echo to source
```

**No relay-role gate.** Every node runs this verbatim. At a *leaf*,
`forward_targets` returns nothing for a third party's voice — no directly-connected
peer has armed interest in `voice/{A}` *at the leaf* (only the relay does, by
broad-subscribing) — so a leaf re-forwards nothing **by construction**, not by an
`if is_relay` check (which has no code home: relay and leaf build identical
engines, distinguished only by broad-subscribing). Flood control for multi-relay
meshes is a hop-limit/spanning-tree concern, deferred with the rest of multi-hop.

This is one branch (`seq > hwm` → forward, else drop). It mirrors `fanout_application_entry`
field-for-field except the gate is the in-memory HWM instead of the store insert.

### One authoritative `seq` per stream, end to end (no denormalization)

An ephemeral **stream** is one `name`; each stream carries its own monotonic
`seq`, stamped on the **envelope** (routing-visible, authenticated) — the
in-payload copy is **deleted in the same change**. The HWM is keyed per
`(sender, name)`, so each stream's seq stays **dense** (no cross-stream gaps).

**Frames and heartbeats become distinct streams** (today they collide on
`voice/{room}/{sender}` — frame seq-bearing, heartbeat seq-less). Frames keep
`voice/{room}/{sender}`; heartbeats move to the subtree
`voice/{room}/{sender}/hb`. Each gets its own per-runtime monotonic counter, so
the frame seq the host worklet consumes stays dense (heartbeats no longer perturb
it). A single `subscribe_via(NamePrefix("voice/{room}/{A}"))` still covers both
(both names share that prefix; the relay routes them as two HWM slots).

1. `SignedDatagram` gains `seq: u64`. `datagram_signing_payload` covers
   `(verifying_key, name, payload, seq)` — frozen field order, `seq` appended.
   **Deliberate pre-1.0 ephemeral wire bump**, retracting the false "no pinned
   vector" claim. As its own reviewed commit, regenerate **all three** affected
   frozen vectors with that rationale: `datagram_payload_frozen_vector` and
   `datagram_payload_excludes_signature_field` (`store/canonical.rs`), and
   `voice_packet_frame_postcard_frozen_vector` (`voice/packet.rs`, which changes
   because `Frame.seq` is removed). Durable `SignedKvEntry`/`ContentBlock` vectors
   stay frozen.
2. The `Bus` seam carries the seq explicitly so there is no second source:
   `publish_ephemeral(name, seq, payload)` (or `Ephemeral { name, seq, payload }`)
   across `Bus` (`bus.rs:42`), `DynBus` (`dyn_bus.rs:14`), `BusImpl` (`bus.rs:97`),
   `dyn_bus_impl`, and both call sites (`mod.rs:219` frame, `heartbeat.rs:61`).
   Stays `#[async_trait(?Send)]`.
3. The voice runtime owns **one monotonic counter per ephemeral stream** (one for
   frames, one for heartbeats), stamps each on its stream's envelope, and
   **removes `VoicePacket::Frame.seq`**. The receive path now sources the seq it
   needs from the **envelope**: `FrameSink::deliver(peer, seq, pcm)` takes the
   frame stream's envelope seq (the on-wire `SignedDatagram.seq`), which survives
   into `subscribe.rs` (it has the `SignedDatagram`, not just the decrypted
   packet); the host worklet's gap-based jitter buffer is unaffected because the
   frame stream stays dense.

No `seq==0` sentinel: the publish path allocates the counter, so relayable
ephemeral always carries a real monotonic seq, and `handle_ephemeral_delivery`
has exactly one branch (`seq > hwm` → forward, else drop), no seen-window.

### HWM lifecycle and locking

Pruned on `PeerRemoved(sender)` (drop that sender's keys); any idle component
folds into the existing routing-refresh tick — **no new timer**. Bounded by
`senders × active ephemeral names`; `u64` does not wrap in a call. The HWM
read-modify-write and the `forward_targets` fan-out both run under the existing
`state.lock()` in `handle_ephemeral_delivery`, matching `publish_ephemeral`'s
existing pattern (holds the lock across sends) — consistent, but it means the
relay takes the engine state lock per inbound datagram, so relay egress/lock
contention is measured in the stress run (see Risks).

### Duplicate delivery — a new receiver-side dedup gate

During switchover a receiver briefly gets a frame both directly and via the
relay. **This dedup does not exist today** — `subscribe.rs` currently inserts
`last_delivered_seq` and calls `deliver` *unconditionally*. The change adds a
real `seq <= last_delivered_seq[sender] → drop` gate (on the envelope seq) before
`deliver`, so each frame is delivered once. A unit/integration test pins "two
interleaved sources, same `(sender, seq)` → exactly one `deliver()`."

---

## Layer 2 — Control plane (voice)

### Provider chosen per call participant, derived from direct presence

Replace voice's **broad** `voice/{room}/` subscription with a **per-participant**
`subscribe_via` whose **provider is a pure function of observed connectivity**:

For each remote participant `A` (local peer `P`):

```
desired_provider(A) = A      if  current_peers() contains (A, Secondary)   // direct
                    = relay   otherwise                                     // fallback
converge: ensure exactly one active subscribe_via(NamePrefix("voice/{room}/{A}"),
          desired_provider(A), ephemeral_policy); withdraw any other provider's.
```

- `provider = A` arms **A's** interest-for-P (A directly forwards its voice to P,
  no relay copy → "prefer WebRTC" and zero relay egress for the pair).
- `provider = relay` arms the **relay's** interest-for-P (relay re-forwards A's
  voice — Layer 1 — to P). The relay receives A's voice because A still floods to
  its connected peers including the relay.

The provider is always reachable by P when chosen: `A` only when a direct link to
A exists; `relay` always (star). So the subscription entry always reaches the
arming node.

**Derived, not toggled.** On every relevant transition
(`PeerAdded/Removed(Secondary)`, membership join/leave) we **recompute
`desired_provider(A)` and converge idempotently** via the already-idempotent
`do_subscribe_via`/`do_unsubscribe_via` (LWW/TTL store writes). A WebRTC flap or a
stale-generation event can never leave the relay subscription wrong: there is no
edge to miss and no debounce — the next recompute reasserts the function's value.
(This is the CLAUDE.md "derive one from the other" rule applied to per-peer state,
and it sidesteps the conn_id/multi-conn-per-peer race surface.)

### Control-plane seams (named, not assumed)

The voice runtime today holds `Rc<dyn DynBus>` + `Rc<dyn Dialer>` and has **zero**
references to `subscribe_via`, `current_peers`, or `EngineEvent`. Three seams are
**new** and are specified here with the rigor the data-plane API change got:

- **Arm/withdraw routing interest.** Add `subscribe_via(filter, provider: PeerId)`
  and `unsubscribe_via(filter, provider)` to the voice-facing engine trait
  (`DynBus`, or a sibling `Routing` trait the runtime also holds). These wrap the
  existing `SyncEngine::subscribe_via`/`unsubscribe_via` (engine.rs:455). `?Send`,
  no new bound.
- **Observe direct-link presence by kind.** Add `current_peers() -> Vec<(PeerId,
  TransportKind)>` and a transition signal to the same trait. For the signal,
  **reuse the engine event stream** (`subscribe_engine_events()` emits
  `PeerAdded{kind}/PeerRemoved`) rather than polling; the runtime recomputes on
  each event. `TransportKind` must be re-exported to `sunset-voice`.
- **Identify the relay provider.** `provider=relay` is the **`Primary` peer** in
  `current_peers()` — WebRTC links are `Secondary`, the relay/WS link is the sole
  `Primary` in the v1 single-relay star. No relay-PeerId injection at construction;
  it is read from the same `current_peers()` the predicate already uses. (If a
  client ever has multiple `Primary` peers, that is the multi-relay case, deferred.)

### Where it lives

A small **per-peer voice-provider component** (sibling to `auto_connect`, sharing
`RuntimeInner`) owns the convergence. It cannot literally fold into `auto_connect`
as first thought: `auto_connect` is driven by durable presence +
`membership_liveness` and has **no `TransportKind` input today**. The new
component subscribes to engine peer-events (the new seam), holds no state beyond
"current desired provider per participant" (itself derived, recomputable from
`current_peers()` + the roster), and converges. Combined per-peer state stays
small: {supervisor dial state (owned elsewhere), desired voice provider (pure
function of `current_peers()` + roster)} — the second is derived, no product blowup.

### Membership / heartbeat reception is preserved (receive ≠ arming)

Dropping voice's broad *BroadcastIntent* subscription changes **remote arming**
(who forwards to me), not **local receive** (what I decode). Keep a **local**
broad `subscribe_ephemeral(NamePrefix("voice/{room}/"))` (in-process,
filter-matched, **no** BroadcastIntent — engine.rs:404) so every frame/heartbeat
*delivered* to this client is decoded and feeds `membership_liveness`
(`in_call`). The per-A `subscribe_via` only controls **which provider forwards**
participant A's streams to me. Bootstrap: the roster comes from durable
**presence** (which already relays via the store), so A is known before its
ephemeral heartbeats flow; the provider component arms A (frames+heartbeats, one
prefix sub) the moment A appears in the roster, choosing provider by
direct-presence, and A's heartbeats then drive `membership_alive`. The pre-arm
window is the same one the current broad BroadcastIntent has (nothing flows
before a peer/relay is armed); presence covers the roster throughout. A Rust test
asserts a relay-only participant's heartbeats reach `membership_liveness` and
`in_call` is set.

### Retry is already correct

The supervisor retries `webrtc://A` forever; the only requirement is that the
voice-scoped dial intent for `A` is **not `cancel_direct`'d while A is in the
call**, so a transient ICE failure self-heals. No "sticky intent" variant, no
kind/stickiness dimension on `IntentEntry`. When the dial finally connects →
`(A, Secondary)` appears → recompute flips `desired_provider(A)` to `A` →
relay subscription withdrawn → audio prefers WebRTC.

---

## Observability (so the e2e can be honest)

Two **first-class, non-test-gated** signals — the proof rests on positive
evidence that audio crossed the relay, never on a test-only inspector or an
inferred negative:

1. **Relay forwarded-ephemeral counter (server-side ground truth).** A monotonic
   count of datagrams Layer 1 actually re-forwarded. Full cross-crate path
   (named, not hand-waved): (a) `EngineState.ephemeral_forwarded: u64`, bumped in
   `handle_ephemeral_delivery` once per datagram that passes the HWM gate and is
   fanned out; (b) a public `SyncEngine::ephemeral_forwarded() -> u64` accessor
   (`sunset-sync`); (c) a field on `DashboardSnapshot` (`sunset-relay/src/bridge.rs`),
   populated by `build_dashboard_snapshot` (`snapshot.rs:37`) via the new accessor;
   (d) surfaced as **JSON** for the Playwright test to `fetch` + parse. Note the
   existing `/dashboard` route renders *plaintext* and the JSON `/` route serves
   `IdentitySnapshot` (no metrics); so add `ephemeral_forwarded` to a JSON
   surface — either extend the `IdentitySnapshot` JSON or add a small
   `/metrics` JSON route — entirely within `sunset-relay`, no cross-layer ripple.
   If audio crossed the relay this rises ≥ N; if WebRTC silently worked it stays
   ~0. Load-bearing honesty signal.
2. **Per-frame inbound provenance (`via = direct | relay`).** Thread the inbound
   `TransportKind` of the connection that delivered an `EphemeralDelivery` down to
   the voice frame recorder, surfaced as a real readout (a "relayed" indicator a
   UI could show — *not* a `window.__` test probe). Lets the receiver assert
   *which* path carried each frame, and the switchover assert that newly-recorded
   frames flip `via=relay` → `via=direct`.

---

## Testing strategy

### Rust unit / integration (deterministic — the correctness proof)

1. **Re-forward, source-excluded.** `handle_ephemeral_delivery` re-forwards a
   newer datagram to a matching peer and **not** back to `from`.
2. **HWM dedup / loop-prevention.** Feed the same datagram twice → forwarded once;
   feed an older seq after a newer → dropped.
3. **Three-engine star, no direct A–B.** A, B, relay; B `subscribe_via(voice/{A},
   provider=relay)`. Assert: B's ephemeral subscriber receives A's datagram, **and
   `A.current_peers()` does not contain B** (receipt explicable only via relay),
   **and the relay's `ephemeral_forwarded` incremented** (re-forward actually
   ran — fails if it reverts to dispatch-local).
4. **Provider convergence via observable consequence.** With `(A, Secondary)`
   present, B's voice for A is armed via `provider=A`; A publishes → B receives,
   relay forward-count flat. Drop the Secondary → recompute → `provider=relay`;
   A publishes → B receives, relay forward-count rises. Re-add Secondary →
   `provider=A` again, relay forward-count flat. (Binds the control decision to a
   real data-plane outcome; uses the runtime's actual per-A subscription shape.)
5. **Datagram wire.** Round-trip; `datagram_signing_payload` covers `seq` (tamper
   `seq` → verify fails); **re-pin the new `datagram_payload_frozen_vector`** and
   the covered-field test.
6. **Dual-delivery dedup.** Two interleaved sources for the same `(sender, seq)` →
   exactly one delivery.

### E2E (Playwright) — honesty is load-bearing

New `web/e2e/voice_relay_fallback.spec.js`, two real browser clients + real
`sunset-relay`, existing voice harness (`voice_inject_pcm`, `voice_recorded_frames`,
`data-voice-connected`), driven through the nix-pinned Playwright env.

- **Block direct WebRTC at the *environment*, not in production code:** supply
  **no / unreachable ICE servers** (via the existing `ice_urls` ctor,
  `wasm.rs:161`) — a genuine ICE failure identical to symmetric-NAT. The block must
  NOT touch `WebDialer` / `connect_direct` / `auto_connect` / the relay subscription
  logic; the supervisor dial machinery runs and *fails*, it is not bypassed.
- **Prove audio crossed the relay (positive):** inject PCM at A; assert B records
  ≥ N decoded frames **and** the relay's `ephemeral_forwarded` rose by ≥ N during
  the window **and** B's recorded frames are tagged `via=relay`. Sub-assert the
  supervisor is **actively retrying** (backoff, not silence) while blocked — proving
  the block models the real failure mode.
- **Prefer-WebRTC + retry (positive):** lift the block; assert a direct `(Secondary)`
  session forms, newly-recorded frames flip to `via=direct` with count climbing,
  and the relay's `ephemeral_forwarded` **stops rising** for the pair (corroborated
  by an on-wire `SubscriptionEntry::Withdrawn` for the relay provider). Then
  re-apply the block and assert fallback again — both directions, within the
  voice-reconnect UX bound (≤ 20s).
- **Forbidden** (project CLAUDE.md): no `wait_for` on engine-internal registries to
  make a user action work; no inspector method gating a user-level action; no
  `sleep` masking a race. The test does what a user does (join, talk, hear); the
  honesty asserts read **first-class observables** (server forward count, frame
  `via` tag, on-wire withdrawal), not `window.__` probes.

### Verification bar

Full workspace tests + clippy + fmt; new Rust tests fail-before/pass-after; e2e
green. Correctness rests on the **positive provenance asserts** (forward count,
`via` tag) proving the right thing. **Then**, per the user's standing
requirement, **CI green 3× in a row**, with `voice_relay_fallback` and
`voice_three_way` confirmed flake-free under an isolated `--repeat-each` stress
(single-run green is not proof at this layer's historical flake rates).

## Non-goals (explicit)

- **Upstream suppression / recursive subscription** — a sender still floods its
  audio to the relay even when every peer is direct (bounded: one stream per
  sender; already today's behavior, now useful). Eliminating it needs the relay to
  subscribe upstream on behalf of downstream peers; deferred.
- **Multi-relay / peer-assisted relaying meshes** — v1 is single-relay star; richer
  topologies need hop-limit/spanning-tree loop control.
- **Congestion control / SFU mixing** — the relay forwards, it does not mix.
- **Changing durable `SignedKvEntry`/`ContentBlock` wire format.**

## Risks

- **Ephemeral wire bump.** Mitigated: pre-1.0, ephemeral-only; the two frozen
  datagram tests are regenerated in a dedicated reviewed commit; durable vectors
  untouched; round-trip + seq-covered tests added.
- **E2E genuineness.** The feature is worthless if the e2e green-passes without
  audio crossing the relay. Mitigated by the **server-side relay forward counter**
  + per-frame `via=relay` tag (positive evidence), plus the environment-level WebRTC
  block (genuine ICE failure) and the "supervisor still retrying" sub-assert. Most
  scrutinized item in code review.
- **Switchover thrash.** Mitigated by deriving the provider (convergent recompute),
  not edge-toggling — flaps/stale generations self-heal.
- **Relay egress.** Bounded by LWW (newest-only) + leaf-forwards-nothing-by-construction;
  measured in the stress run.
