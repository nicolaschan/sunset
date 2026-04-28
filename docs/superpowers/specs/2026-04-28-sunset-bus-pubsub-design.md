# Bus pub/sub — Design

**Goal:** unify durable (CRDT-replicated) and ephemeral (real-time, fire-and-forget) message delivery behind a single `Bus` API in sunset-core. Same subscription filter system, same signing model, same namespace; the only difference is persistence + transport channel.

**Non-goals:** voice itself (separate plan), forwarding via intermediate peers (separate plan), implementing the unreliable WebRTC datachannel (separate plan that this one composes with), replay protection (application-layer concern).

---

## Background

After the UI presence + peer-status work, sunset-core has Identity / Room / encrypted message composition and consumes sunset-sync's engine + sunset-store's CRDT store. Application code (chat) calls `store.insert(SignedKvEntry)` to publish; the engine's subscription registry routes the entry to peers whose `Filter` matches; receivers see it via `store.subscribe(filter)`.

Voice (and future real-time payloads) wants the same conceptual contract — *publish on a namespace; subscribers in that namespace receive* — but with different physical guarantees: no persistence, fire-and-forget, low latency, delivered over the unreliable WebRTC datachannel.

This spec introduces a `Bus` abstraction in sunset-core that exposes both delivery modes under one symmetric API and reuses the existing CRDT-backed subscription registry for routing.

---

## Architecture

```
                ┌──────────────────────┐
   Application──▶  Bus (sunset-core)   ◀── Application
                │                      │
                │ publish_durable      │
                │ publish_ephemeral    │
                │ subscribe            │
                └──────────────────────┘
                  │            │
       (durable)  ▼            ▼  (ephemeral)
            ┌─────────┐    ┌────────────────┐
            │  Store  │    │ sunset-sync    │
            │  (CRDT) │    │ ephemeral      │
            └────┬────┘    │ delivery       │
                 │         └────┬───────────┘
                 │              │
         CRDT replication       SyncMessage::EphemeralDelivery
         (reliable channel)     (unreliable channel)
                 │              │
                 ▼              ▼
            (other peers' Bus subscribers)
```

The two sides share: subscription registry, signing model, namespace, fan-out logic. They differ only in: persistence, retry semantics, transport channel.

---

## Types

### `SignedDatagram` (new, in `sunset-store`)

Lives in `sunset-store` next to `SignedKvEntry`, because it shares the canonical-encoding + verifier infrastructure and is part of the wire format the store/sync layer carries.

```rust
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedDatagram {
    pub verifying_key: VerifyingKey,
    pub name: Bytes,         // namespace; same shape as SignedKvEntry::name
    pub payload: Bytes,      // inline application bytes
    pub signature: Bytes,    // Ed25519 over canonical(verifying_key, name, payload)
}
```

**Why inline payload (no `value_hash` / `ContentBlock`):** durable entries reference content blocks because the same value can be re-shared by hash. Ephemeral has no reuse — the value IS the message. Indirection adds round-trips for no benefit.

**Fields that don't apply** vs. `SignedKvEntry`: no `priority` (no LWW; the "newest" is just the one that arrived most recently), no `expires_at` (delivery == lifetime; no storage to expire from).

### Canonical signing payload

```rust
// sunset-store::canonical
pub fn datagram_signing_payload(d: &SignedDatagram) -> Vec<u8>;
```

Postcard encoding of `(verifying_key, name, payload)` — same pattern as `signing_payload(SignedKvEntry)`. Frozen by a hex-pinned test vector in `sunset-store/src/canonical.rs`.

### `Bus` trait (new, in `sunset-core`)

```rust
#[async_trait(?Send)]
pub trait Bus {
    /// Publish a durable entry. Engine replicates via CRDT to peers
    /// whose subscription filter matches.
    async fn publish_durable(
        &self,
        entry: SignedKvEntry,
        block: Option<ContentBlock>,
    ) -> Result<()>;

    /// Sign + fan-out an ephemeral datagram to peers whose subscription
    /// filter matches `name`. Fire-and-forget; no persistence.
    /// Delivered best-effort over the unreliable transport channel.
    async fn publish_ephemeral(
        &self,
        name: Bytes,
        payload: Bytes,
    ) -> Result<()>;

    /// Subscribe to messages matching `filter` across both delivery
    /// modes. The subscription is automatically published into the
    /// CRDT subscription registry so peers will route both durable
    /// and ephemeral messages matching the filter to us.
    async fn subscribe(
        &self,
        filter: Filter,
    ) -> Result<LocalBoxStream<'static, BusEvent>>;
}

#[derive(Debug)]
pub enum BusEvent {
    Durable {
        entry: SignedKvEntry,
        block: Option<ContentBlock>,
    },
    Ephemeral(SignedDatagram),
}
```

### `SyncMessage::EphemeralDelivery` (new, in `sunset-sync`)

```rust
pub enum SyncMessage {
    // ... existing variants ...
    EphemeralDelivery { datagram: SignedDatagram },
}
```

Frozen by a hex-pinned test vector in `sunset-sync/src/message.rs`.

---

## Components

### sunset-store additions

- `SignedDatagram` struct + serde derive.
- `canonical::datagram_signing_payload` helper.
- Hex-pinned test vector for the canonical encoding.

The store backend is **not** modified — `Store` remains a CRDT abstraction. `SignedDatagram` is a wire-format/types-only addition.

### sunset-sync additions

- `SyncMessage::EphemeralDelivery { datagram }` variant.
- New `pub fn publish_ephemeral(datagram: SignedDatagram) -> Result<()>` on `SyncEngine` that:
  1. Looks up the subscription registry for peers whose filter matches `datagram.name`.
  2. Sends `SyncMessage::EphemeralDelivery { datagram }` to each match's per-peer outbound queue, with a flag indicating "use unreliable channel."
  3. **Locally**: dispatches the datagram to in-process subscribers whose filter matches (so a single-process publish+subscribe setup works for tests + when the sender is also a subscriber).
- Per-peer outbound flow: when send_message gets a SyncMessage that's flagged unreliable, it calls `conn.send_unreliable(...)` instead of `conn.send_reliable(...)`. Other messages stay on reliable.
- Per-peer inbound flow: a separate task drains `conn.recv_unreliable()` in parallel with `conn.recv_reliable()`. Both feed into the same `InboundEvent::Message { from, message }` channel; the engine doesn't care which physical channel a SyncMessage arrived on.
- Inbound `EphemeralDelivery` handling: verify `datagram.signature` against the configured `SignatureVerifier` (Ed25519). Drop on failure (log at warn). Dispatch to local subscribers whose filter matches.
- Engine maintains a per-ephemeral-subscriber dispatch table: `Vec<(Filter, UnboundedSender<SignedDatagram>)>`. New API:

  ```rust
  pub async fn subscribe_ephemeral(
      &self,
      filter: Filter,
  ) -> UnboundedReceiver<SignedDatagram>;
  ```

  Returns the **raw** `SignedDatagram` stream. The engine doesn't know about `BusEvent` — that wrapping happens in sunset-core (correct dependency direction: sunset-core → sunset-sync, never the reverse).

**Routing decision rule (durable vs ephemeral, send side):**

```
fn outbound_kind(msg: &SyncMessage) -> ChannelKind {
    match msg {
        SyncMessage::EphemeralDelivery { .. } => Unreliable,
        _ => Reliable,
    }
}
```

### sunset-core additions

- `Bus` trait.
- `BusImpl` (or similar) — concrete impl that wraps `Arc<Engine>` + `Arc<Store>` + `Arc<Identity>`. Provides:
  - `publish_durable` → delegates to `store.insert` + relies on engine's existing local_sub fan-out.
  - `publish_ephemeral` → builds `SignedDatagram`, signs via `identity.sign(canonical(...))`, calls `engine.publish_ephemeral(datagram)`.
  - `subscribe` → calls `engine.publish_subscription(filter, ttl)` (so peers learn we want this filter), opens `store.subscribe(filter, Replay::All)` for the durable side, opens `engine.subscribe_ephemeral(filter)` for the ephemeral side, then merges the two:
    - Store events: keep `Inserted` and `Replaced { new, .. }` → fetch the entry's content block via `store.get_content` if needed, emit `BusEvent::Durable { entry, block }`. Drop `Expired`/`BlobAdded`/`BlobRemoved` (not application-relevant for the bus surface).
    - Ephemeral events: each `SignedDatagram` becomes `BusEvent::Ephemeral(datagram)`.
    - Merged into one `LocalBoxStream<'static, BusEvent>`.

### Identity / signing

`SignedDatagram` is signed by the publisher's Ed25519 secret. Verifier on receive is the same `SignatureVerifier` configured on the receiving side's store / engine. For the v1 deployment that's `Ed25519Verifier` (matches durable entries).

**Why sign every packet:**
- Forwarders (V_forwarding plan, future) can verify before re-broadcasting; can't be tricked into propagating spoofed traffic.
- Receivers can trust origin without trusting the path the packet took.
- Maintains the symmetry with `SignedKvEntry`: anyone can verify, no shared-secret burden.

**Performance note:** voice is ~50 packets/sec. Ed25519 sign ≈ 50 µs, verify ≈ 150 µs (Skylake). 50 Hz signing is ~2.5 ms/sec CPU on the sender — fine. Receiver verifies one packet per source — also fine. Higher rates (e.g. screen share at 1 kHz) would warrant a faster MAC, but v1 doesn't need it.

**Signature size:** 64 bytes per packet. Voice frames at 20 ms / Opus 16 kbps are ~40 bytes, so signature roughly doubles wire size. Acceptable for a chat app's voice; revisit if bandwidth becomes a problem.

---

## Data flow

### Publish durable (chat send, today's flow, no change)

1. App: `bus.publish_durable(entry, Some(block))`.
2. Bus delegates to `store.insert(entry, Some(block))`.
3. Store fires local subscription event.
4. Engine's `local_sub.next()` arm runs `handle_local_store_event`, fans out to peers via `EventDelivery` over reliable channel.

### Publish ephemeral (voice send, new)

1. App: `bus.publish_ephemeral(name, payload)`.
2. Bus signs: `let datagram = SignedDatagram { verifying_key: id.pub, name, payload, signature: id.sign(canonical(...)) }`.
3. Bus calls `engine.publish_ephemeral(datagram)`.
4. Engine consults `SubscriptionRegistry` — for each peer whose filter matches `name`:
   - Send `SyncMessage::EphemeralDelivery { datagram }` over peer's unreliable channel.
5. Locally: engine also dispatches `BusEvent::Ephemeral(datagram.clone())` to local subscribers whose filter matches (loopback delivery).

### Subscribe (mixed stream)

1. App: `let stream = bus.subscribe(filter).await?;`
2. Bus calls `engine.publish_subscription(filter, ttl)` — peers learn what we want via the existing CRDT mechanism.
3. Bus opens TWO inner streams:
   - Durable: `store.subscribe(filter, Replay::All)` — receives `Event::Inserted` / `Event::Replaced` for entries matching the filter.
   - Ephemeral: `engine.subscribe_ephemeral(filter)` — receives in-process ephemeral dispatches as raw `SignedDatagram`.
4. Bus merges them into one `LocalBoxStream<'static, BusEvent>`:
   - Store `Inserted(entry)` / `Replaced { new, .. }` → `BusEvent::Durable { entry, block }` (block fetched lazily via `store.get_content` if not already in hand).
   - `SignedDatagram` → `BusEvent::Ephemeral(datagram)`.
   - Other store event variants (`Expired`, `BlobAdded`, `BlobRemoved`) are dropped — not application-relevant for the bus.
5. App reads from the merged stream; matches on `BusEvent` variants.

### Receive ephemeral (other side)

1. Per-peer task drains `conn.recv_unreliable()` continuously.
2. Decoded SyncMessage = `EphemeralDelivery { datagram }`.
3. Engine: verify `datagram.signature` against `SignatureVerifier`. Drop on failure (warn).
4. Engine: look up bus subscribers whose filter matches `datagram.name`. Send `BusEvent::Ephemeral(datagram)` to each subscriber's stream.

---

## Subscription registry reuse

The existing `SubscriptionRegistry` in sunset-sync is unchanged. Each peer publishes ONE filter via the durable entry under `_sunset-sync/subscribe`. The engine routes:

- Durable entries (today): if filter matches, send `EventDelivery` over reliable.
- Ephemeral datagrams (new): if filter matches, send `EphemeralDelivery` over unreliable.

Application code that calls `bus.subscribe(filter)` ensures the filter is registered — same call as today's `engine.publish_subscription(filter, ttl)`. No new subscription protocol.

---

## Wire format changes

Two new postcard encodings, both frozen with hex test vectors:

1. `SignedDatagram` (in sunset-store) — see `datagram_signing_payload` for canonical bytes.
2. `SyncMessage::EphemeralDelivery` (in sunset-sync) — postcard variant tag + nested `SignedDatagram`.

Both must round-trip across browsers/native; both gate on test vectors that fail loud if encoding drifts.

---

## Failure modes

| Failure | Behavior |
|---|---|
| `publish_ephemeral` with no matching peers | Returns `Ok(())`. Datagram dropped silently — same as no listener. Loopback to local subscribers still happens. |
| Subscribe-late race: peer publishes ephemeral before our subscription has propagated through the registry | Some initial datagrams missed silently. **Inherent to pub/sub; no mitigation in this layer.** Applications that need "I'm ready to receive" guarantees should layer a reliable handshake on top of the bus (e.g., the voice plan sends a small durable "subscribed" marker before the publisher starts streaming). The bus does NOT block subscribe on registry propagation, because (a) the registry is broadcast to N peers and acks from any subset prove nothing about the others, (b) the receiver-side window between registry-arrival and routing-table-update is still there even after an ack, and (c) standard pub/sub semantics don't promise instantaneous join — replay or handshake is the consumer's choice. |
| Matching peer has no unreliable channel (relay-only WS connection) | Skipped silently in v1; logged at debug. **V_forwarding addresses this.** |
| Signature verification fails on receive | Drop datagram; log at warn. |
| Unreliable send fails (queue full, channel closed) | Drop; log at debug. Per-peer task continues. |
| Unreliable recv stream errors / closes | Per-peer task transitions to `Disconnected` (same as reliable). |
| Replay (same datagram delivered twice) | Application's responsibility — embed seq/timestamp in `payload` if you care. |
| Datagram payload too large for unreliable MTU (~16 KB on RTCDataChannel) | Caller's responsibility to fragment. v1 documented limit; voice frames are ~40-200 bytes so not an issue. |

---

## Testing

### sunset-store unit tests
- `datagram_signing_payload` round-trip: build datagram, encode, sign with Ed25519, verify.
- Frozen test vector: `blake3(datagram_signing_payload(sample))` matches a hex constant.

### sunset-sync unit tests
- `SyncMessage::EphemeralDelivery` postcard round-trip + frozen test vector.
- Two-peer integration test (using `TestTransport` with simulated unreliable channel — mirrors the existing two-peer reliable test):
  - A subscribes, B publishes ephemeral matching A's filter → A receives.
  - B publishes ephemeral NOT matching A's filter → A doesn't receive.
  - B publishes with bad signature → A drops, no event delivered.

### sunset-core unit tests
- `Bus` impl: in-process publish_ephemeral with a local subscriber → receives via loopback.
- `Bus` impl: publish_durable still works through the same surface.
- `Bus::subscribe` returns a stream that yields both `Durable` and `Ephemeral` events in arrival order.

### Integration test (Rust, native)
- Two engines connected via `TestTransport` (which gains unreliable support — see Plan A).
- Both call `bus.subscribe(filter)`.
- One side calls `bus.publish_ephemeral(name, payload)`; other side receives `BusEvent::Ephemeral` matching the filter.

### Browser end-to-end (deferred)
Voice plan (Plan C) covers the browser e2e of ephemeral over real WebRTC. This spec doesn't add Playwright tests directly; the bus is exercised via Rust tests + the future voice plan.

---

## Layer boundary — JS / Gleam surface

**This plan adds zero JS / Gleam surface.** The Bus is a Rust-internal API consumed only by Rust callers (`sunset-core`'s chat code today; the future voice plan tomorrow; eventually `sunset-tui`, `sunset-relay`, `sunset-mod`).

This plan also pins a load-bearing invariant for downstream plans: **all business logic — routing, signing, framing, codec, packet sequencing, AEAD, jitter handling — lives in Rust crates**. The Gleam UI and JS FFI shim should be purely plumbing:

| Layer | Owns | Surfaces to JS/Gleam |
|---|---|---|
| sunset-store | CRDT, content-addressed blobs, signing canonical encoding | none |
| sunset-sync | transports, replication, subscription registry, ephemeral routing | none |
| sunset-core | Identity, Room, message composition, **Bus**, future voice session lifecycle, Opus, framing, AEAD | none |
| sunset-web-wasm | thin wasm-bindgen wrappers; `Client` class with one method per high-level user action | only what's listed here |
| Gleam UI / sunset.ffi.mjs | render state; dispatch user events; wire MediaStream → wasm pipe; wire decoded samples → AudioContext; URL params; localStorage | UI state, DOM, MediaStream/AudioContext glue |

When the future voice plan lands, the rule it must obey: the Gleam/JS code paths are limited to (a) MediaStream/AudioContext I/O glue, (b) one-line method calls into the Rust Client per user action, (c) callback registration for state and decoded audio. Encoding, sequencing, peer selection, retry, etc., never leak above the wasm boundary.

## Out of scope

- **Implementing the unreliable channel itself** on `WebRtcRawConnection` — that's Plan A. Bus assumes the channel works on every transport that exposes one. WebSocket transport's `send_unreliable` continues to return `Err`; ephemeral delivery to relay-only peers will be skipped in v1.
- **Forwarding via intermediate peers** (Plan V_forwarding). The Bus will compose with that later: a forwarding peer is just a regular peer whose subscription filter matches and who calls `bus.publish_ephemeral` again after verifying the signature.
- **Replay protection** — application-layer concern; voice payloads will carry their own monotonic seq.
- **Voice-specific encoding (Opus, framing)** — separate plan (Plan C). Bus is payload-agnostic.
- **Rate limiting / backpressure** — v1 relies on the unreliable channel's natural drop semantics. Revisit if a misbehaving sender saturates a peer.
- **TestTransport unreliable simulation** — minimal impl needed for sunset-sync's integration tests, but rich loss/jitter modeling is deferred.

---

## Sequencing within voice work

```
Plan A: Unreliable channel impl   ─┐
                                   ├─→  Plan C: Voice end-to-end
Plan B: Bus (this spec)            ─┘
```

A and B are independent; B is testable using `TestTransport` augmented with a basic unreliable channel. C depends on both.

---

## Naming notes

- `Bus` is a working name. Alternatives considered: `PubSub`, `Topics`, `Channel`. Stick with `Bus` unless a clearer name surfaces during implementation; it's a small enough surface to rename in a follow-up.
- `EphemeralDelivery` is the SyncMessage variant name. Mirrors `EventDelivery` (durable) for symmetry.
- `BusEvent::Durable` / `BusEvent::Ephemeral` are the consumer-facing variants.
