# Relays list and metrics — implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the dummy "minecraft bridge" channels-rail entry with a real "Relays" list driven by the supervisor's per-intent snapshot stream, including live last-pong-at and round-trip-time per relay. Click-through opens a popover (floating on desktop, bottom sheet on phone) with full metrics.

**Architecture:** Three layers change; the wire format does not. (1) `sunset-sync` measures RTT in the existing per-peer liveness loop and emits `EngineEvent::PongObserved`, which the supervisor folds into `IntentSnapshot.{last_pong_at_unix_ms, last_rtt_ms}`. (2) The wasm bridge enriches `intent_snapshot_to_js` with the new fields and accepts a `heartbeat_interval_ms` constructor parameter (e2e-only). (3) The Gleam app subscribes to `on_peer_connection_state` at Client construction, filters relay-scheme intents, and renders a "Relays" rail section + popover that mirrors `peer_status_popover`'s desktop/phone-sheet placement.

**Tech Stack:** Rust workspace (stable), tokio (current_thread), `web_time::SystemTime` for cross-platform wall-clock, wasm-bindgen, Gleam (lustre), Playwright.

**Spec:** [`docs/superpowers/specs/2026-05-03-relays-list-and-metrics-design.md`](../specs/2026-05-03-relays-list-and-metrics-design.md)

---

## File structure

**Modify (Rust — sunset-sync):**
- `crates/sunset-sync/src/peer.rs` — change `pong_tx`/`pong_rx` carry to `web_time::Instant` send-time; in the liveness loop, on Pong receipt compute `rtt_ms = now − send_time` and emit `InboundEvent::PongObserved`.
- `crates/sunset-sync/src/engine.rs` — new `InboundEvent::PongObserved` variant; new `EngineEvent::PongObserved`; `handle_inbound_event` re-emits to engine subscribers.
- `crates/sunset-sync/src/supervisor.rs` — add `last_pong_at_unix_ms` / `last_rtt_ms` to `IntentEntry` and `IntentSnapshot`; new `EngineEvent::PongObserved` arm in `handle_engine_event`; update `Snapshot` command and `broadcast` to include new fields; preserve fields across Backoff transitions; clear on `Remove`.

**Modify (Rust — sunset-web-wasm):**
- `crates/sunset-web-wasm/src/client.rs` — `Client::new` accepts a `heartbeat_interval_ms: u32` (0 = use default) and writes through to `SyncConfig`; `intent_snapshot_to_js` writes `last_pong_at_unix_ms` and `last_rtt_ms` when `Some`.

**Modify (Gleam):**
- `web/src/sunset_web/domain.gleam` — add `Relay`, `RelayConnState`; remove `Bridge(_)` from `ChannelKind`, remove the `BridgeOpt`/`HasBridge`/`NoBridge`/`BridgeKind`/`Minecraft` types and the `bridge:` field from `Room`/`Member`/`Message`.
- `web/src/sunset_web/fixture.gleam` — drop `minecraft-bridge` channel and all `bridge:` field usage.
- `web/src/sunset_web/views/main_panel.gleam` — drop `bridge_tag` rendering and the `case m.bridge` branch.
- `web/src/sunset_web/views/channels.gleam` — replace the `bridge_channels` filter and "Bridges" `section(...)` with a call to `relays.rail_section(...)`. Add `relays` parameter to `view`.
- `web/src/sunset_web/sunset.gleam` — declare new externals: `subscribe_peer_connections`, `peer_connection_snapshot`, `heartbeat_interval_ms_from_url`, plus a record type for the JS-side intent snapshot accessors.
- `web/src/sunset_web/sunset.ffi.mjs` — add `subscribePeerConnections`, `peerConnectionSnapshot`, `heartbeatIntervalMsFromUrl` JS shims; pass the URL-param value as second arg to `new Client(seed, heartbeatIntervalMs)`.
- `web/src/sunset_web.gleam` — Model gains `relays: List(Relay)` and `relays_popover: Option(String)`; new Msgs `PeerConnectionSnapshotSeed`, `PeerConnectionStateUpdated`, `OpenRelayPopover(String)`, `CloseRelayPopover`; init effect seeds + subscribes; channels.view gets `relays` and `OpenRelayPopover` callback; popover overlay branches mounted alongside `peer_status_popover_overlay` (Floating desktop, in-bottom_sheet phone).

**Create (Rust):**
- New unit tests inline in `crates/sunset-sync/src/peer.rs`, `engine.rs`, and `supervisor.rs`.

**Create (Gleam):**
- `web/src/sunset_web/views/relays.gleam` — `rail_section` + `popover` (with `Placement = Floating | InSheet`), plus pure helpers `parse_host`, `is_relay_addr`, `format_status`, `format_rtt`, `humanize_age` (the last copied — kept duplicated per the spec rather than pre-extracting a shared helper).
- `web/test/sunset_web/views/relays_test.gleam` — pure-function tests.
- `web/e2e/relays.spec.js` — Playwright e2e covering desktop + phone bottom-sheet placement.

---

## Task 1 — Add `EngineEvent::PongObserved` and `InboundEvent::PongObserved`

**Files:**
- Modify: `crates/sunset-sync/src/engine.rs:90-98` (add EngineEvent variant)
- Modify: `crates/sunset-sync/src/peer.rs:15-52` (add InboundEvent variant)

- [ ] **Step 1.1: Add `InboundEvent::PongObserved` variant**

In `crates/sunset-sync/src/peer.rs`, append a variant to the `InboundEvent` enum:

```rust
    /// A `Pong` was received from a peer; carries the round-trip time
    /// measured against the most recent `Ping` send and the wall-clock
    /// instant the Pong was observed. Engine re-emits as
    /// `EngineEvent::PongObserved` for supervisor / UI consumption.
    PongObserved {
        peer_id: PeerId,
        rtt_ms: u64,
        observed_at_unix_ms: u64,
    },
```

- [ ] **Step 1.2: Add `EngineEvent::PongObserved` variant**

In `crates/sunset-sync/src/engine.rs:90-98`, append:

```rust
    /// A liveness `Pong` round-tripped from a connected peer. Carries
    /// the measured RTT and the wall-clock time the Pong was observed.
    /// Subscribers (e.g. `PeerSupervisor`) use this to surface live
    /// per-peer health to applications. Fired once per heartbeat per
    /// peer (default cadence: every `heartbeat_interval`, 15 s).
    PongObserved {
        peer_id: PeerId,
        rtt_ms: u64,
        observed_at_unix_ms: u64,
    },
```

- [ ] **Step 1.3: Verify the workspace still builds (no handlers yet)**

Run: `nix develop --command cargo build -p sunset-sync 2>&1 | tail -10`
Expected: builds cleanly. The `match`es over `InboundEvent` are non-exhaustive at all current call sites except `handle_inbound_event`; add it there in Task 3.

- [ ] **Step 1.4: Commit**

```bash
git add crates/sunset-sync/src/engine.rs crates/sunset-sync/src/peer.rs
git commit -m "$(cat <<'EOF'
sunset-sync: add InboundEvent and EngineEvent PongObserved variants

Carries per-peer liveness (RTT and wall-clock observed-at) so the
supervisor can surface it to applications via IntentSnapshot. No
producer or consumer yet — wired in subsequent tasks.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 2 — Liveness loop measures RTT and emits `InboundEvent::PongObserved`

**Files:**
- Modify: `crates/sunset-sync/src/peer.rs` (`recv_reliable_task` and `liveness_task` — change pong channel payload, send PongObserved)

- [ ] **Step 2.1: Change `pong_tx`/`pong_rx` to carry the receive instant + nonce**

In `crates/sunset-sync/src/peer.rs:186`, replace the `pong_tx` channel definition:

```rust
    // Pong delivery channel: recv_reliable_task forwards every observed
    // Pong here so the liveness_task can update last_pong_at AND emit
    // PongObserved with measured RTT, without sharing mutable state
    // across tasks. The `nonce` is the Pong's echoed nonce — informational
    // for logs; RTT is measured against `last_ping_sent_at` in the
    // liveness loop, which is correct because at most one Ping is in
    // flight (loop sleeps `heartbeat_interval` between sends).
    let (pong_tx, mut pong_rx) = mpsc::unbounded_channel::<u64>();
```

In `recv_reliable_task` (around line 216-219), change the Pong handler:

```rust
                    Ok(SyncMessage::Pong { nonce }) => {
                        // Notify liveness_task with the echoed nonce.
                        let _ = pong_tx.send(nonce);
                    }
```

- [ ] **Step 2.2: Track ping send time and emit PongObserved on receipt**

Replace the entire `liveness_task` block in `crates/sunset-sync/src/peer.rs:305-359` with:

```rust
    let liveness_task = {
        let inbound_tx = inbound_tx.clone();
        let peer_id = peer_id.clone();
        let out_tx_for_ping = out_tx_clone.clone();
        async move {
            let mut next_nonce: u64 = 1;

            // Cross-platform monotonic clock for RTT and last-pong age.
            #[cfg(not(target_arch = "wasm32"))]
            use tokio::time::Instant;
            #[cfg(target_arch = "wasm32")]
            use wasmtimer::std::Instant;

            let mut last_pong_at: Instant = Instant::now();
            // Time we sent the most recent Ping. Some only between Ping
            // send and corresponding Pong receipt (or the next Ping
            // send, whichever comes first). At most one Ping is in
            // flight because the loop sleeps `heartbeat_interval`.
            let mut last_ping_sent_at: Option<Instant> = None;

            loop {
                #[cfg(not(target_arch = "wasm32"))]
                let tick = tokio::time::sleep(heartbeat_interval);
                #[cfg(target_arch = "wasm32")]
                let tick = wasmtimer::tokio::sleep(heartbeat_interval);

                tokio::select! {
                    _ = tick => {
                        if out_tx_for_ping
                            .send(SyncMessage::Ping { nonce: next_nonce })
                            .is_err()
                        {
                            return;
                        }
                        last_ping_sent_at = Some(Instant::now());
                        next_nonce = next_nonce.wrapping_add(1);

                        let now = Instant::now();
                        if now.duration_since(last_pong_at) > heartbeat_timeout {
                            let _ = inbound_tx.send(InboundEvent::Disconnected {
                                peer_id: peer_id.clone(),
                                conn_id,
                                reason: "heartbeat timeout".into(),
                            });
                            return;
                        }
                    }
                    Some(_nonce) = pong_rx.recv() => {
                        let now = Instant::now();
                        last_pong_at = now;
                        let rtt_ms = match last_ping_sent_at.take() {
                            Some(sent) => now.duration_since(sent).as_millis() as u64,
                            // Pong with no in-flight Ping (peer-initiated
                            // probe, replay, or post-disconnect race). RTT
                            // is undefined; clamp to 0 so we still update
                            // last_pong_at and surface a heartbeat.
                            None => 0,
                        };
                        let observed_at_unix_ms = web_time::SystemTime::now()
                            .duration_since(web_time::UNIX_EPOCH)
                            .map(|d| d.as_millis() as u64)
                            .unwrap_or(0);
                        let _ = inbound_tx.send(InboundEvent::PongObserved {
                            peer_id: peer_id.clone(),
                            rtt_ms,
                            observed_at_unix_ms,
                        });
                    }
                    else => return,
                }
            }
        }
    };
```

- [ ] **Step 2.3: Compile-check**

Run: `nix develop --command cargo build -p sunset-sync 2>&1 | tail -10`
Expected: builds. (The new `InboundEvent` variant still hits a non-exhaustive `match` in `handle_inbound_event`; we wire that in the next task.)

If it errors with "non-exhaustive patterns: `PongObserved { .. }` not covered", add a temporary `InboundEvent::PongObserved { .. } => {}` arm in `handle_inbound_event` so this task compiles independently. Task 3 replaces it with the real handler.

- [ ] **Step 2.4: Add a peer-task unit test**

Append to the `#[cfg(test)] mod tests` block in `crates/sunset-sync/src/peer.rs`. (If no such block exists, create it at the file's end.) The test drives the per-peer task with the existing `test_transport` fixtures and asserts a `PongObserved` event lands on `inbound_rx` after the peer task receives a Pong:

```rust
#[cfg(test)]
mod liveness_tests {
    use super::*;
    use crate::engine::ConnectionId;
    use crate::test_transport::{InMemoryConn, InMemoryPair};
    use crate::transport::TransportKind;
    use std::time::Duration;
    use sunset_store::VerifyingKey;

    fn peer(b: &[u8]) -> PeerId {
        PeerId(VerifyingKey::new(Bytes::copy_from_slice(b)))
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn liveness_emits_pong_observed_with_rtt() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let pair = InMemoryPair::new(TransportKind::Secondary);
                let local_id = peer(b"local-aaaaaaaaaaaaaaaaaaaaaaaaaa");
                let remote_id = peer(b"remote-aaaaaaaaaaaaaaaaaaaaaaaa");
                let env = PeerEnv {
                    local_peer: local_id.clone(),
                    protocol_version: crate::types::SyncConfig::default().protocol_version,
                    heartbeat_interval: Duration::from_millis(20),
                    heartbeat_timeout: Duration::from_secs(5),
                };
                let conn_id = ConnectionId(1);
                let (inbound_tx, mut inbound_rx) = mpsc::unbounded_channel::<InboundEvent>();
                let (out_tx, outbound_rx) = mpsc::unbounded_channel::<SyncMessage>();
                let conn = std::rc::Rc::new(pair.local);
                tokio::task::spawn_local(run_peer(
                    conn,
                    env,
                    conn_id,
                    out_tx,
                    outbound_rx,
                    inbound_tx,
                    None,
                ));
                // Peer-side: respond to Hello with Hello, then echo Ping → Pong.
                pair.remote
                    .feed(crate::message::SyncMessage::Hello {
                        protocol_version: crate::types::SyncConfig::default().protocol_version,
                        peer_id: remote_id.clone(),
                    })
                    .await;
                // Drain Hello from local outbound side.
                let _hello_out = pair.remote.drain_one().await.expect("hello");
                // Wait for first Ping; respond with Pong of the same nonce.
                tokio::time::advance(Duration::from_millis(25)).await;
                let ping = pair.remote.drain_one().await.expect("ping");
                let nonce = match ping {
                    crate::message::SyncMessage::Ping { nonce } => nonce,
                    other => panic!("expected Ping, got {other:?}"),
                };
                pair.remote
                    .feed(crate::message::SyncMessage::Pong { nonce })
                    .await;
                // Drain inbound until we see a PongObserved.
                let mut found = None;
                for _ in 0..32 {
                    if let Ok(Some(ev)) = tokio::time::timeout(
                        Duration::from_millis(50),
                        inbound_rx.recv(),
                    ).await {
                        if let InboundEvent::PongObserved { rtt_ms, peer_id, .. } = &ev {
                            assert_eq!(peer_id, &remote_id);
                            found = Some(*rtt_ms);
                            break;
                        }
                    }
                }
                let rtt = found.expect("no PongObserved seen");
                // RTT is non-negative by type; assertion documents the contract.
                assert!(rtt < 5_000, "RTT should be small under paused time");
            })
            .await;
    }
}
```

If `test_transport`'s `InMemoryPair`/`feed`/`drain_one` shape differs from what's used here, adjust to the fixture's actual API (read `crates/sunset-sync/src/test_transport.rs`). The test's contract — drive Hello → expect Ping → respond Pong → assert PongObserved with non-zero `rtt_ms` — does not change.

- [ ] **Step 2.5: Run the test**

Run: `nix develop --command cargo test -p sunset-sync liveness_tests::liveness_emits_pong_observed_with_rtt -- --nocapture 2>&1 | tail -25`
Expected: PASS. If the assertion `assert!(rtt < 5_000, ...)` fails because `tokio::time::advance` skipped past the heartbeat, that's still a meaningful PongObserved — relax the upper bound or remove if needed, but DO NOT relax the "non-negative" property.

- [ ] **Step 2.6: Commit**

```bash
git add crates/sunset-sync/src/peer.rs
git commit -m "$(cat <<'EOF'
sunset-sync: liveness measures RTT and emits PongObserved

The per-peer liveness loop now stamps last_ping_sent_at on each Ping
and computes rtt = now - sent on Pong receipt, emitting
InboundEvent::PongObserved with rtt_ms and wall-clock observed_at_unix_ms.

The pong channel carries the echoed nonce (informational); RTT is
measured against the most recent send time, which is correct as long
as at most one Ping is in flight — the heartbeat-interval sleep
between sends guarantees that.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 3 — Engine re-emits `InboundEvent::PongObserved` as `EngineEvent::PongObserved`

**Files:**
- Modify: `crates/sunset-sync/src/engine.rs:481-` (`handle_inbound_event`)

- [ ] **Step 3.1: Add the handler arm**

In `crates/sunset-sync/src/engine.rs`, locate the `match event` block in `handle_inbound_event`. Add (or replace the placeholder from Task 2.3):

```rust
            InboundEvent::PongObserved { peer_id, rtt_ms, observed_at_unix_ms } => {
                self.emit_engine_event(EngineEvent::PongObserved {
                    peer_id,
                    rtt_ms,
                    observed_at_unix_ms,
                })
                .await;
            }
```

- [ ] **Step 3.2: Add an engine-level test that PongObserved propagates**

Append to the existing tests module in `crates/sunset-sync/src/engine.rs`:

```rust
#[tokio::test(flavor = "current_thread")]
async fn pong_observed_inbound_event_propagates_as_engine_event() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (engine, _store) = test_helpers::make_engine().await;
            let mut subs = engine.subscribe_engine_events().await;
            let pid = PeerId(sunset_store::VerifyingKey::new(
                bytes::Bytes::from_static(&[7u8; 32]),
            ));
            engine
                .handle_inbound_event(InboundEvent::PongObserved {
                    peer_id: pid.clone(),
                    rtt_ms: 42,
                    observed_at_unix_ms: 1_700_000_000_000,
                })
                .await;
            let ev = tokio::time::timeout(std::time::Duration::from_millis(200), subs.recv())
                .await
                .expect("no engine event")
                .expect("subscriber closed");
            match ev {
                EngineEvent::PongObserved { peer_id, rtt_ms, observed_at_unix_ms } => {
                    assert_eq!(peer_id, pid);
                    assert_eq!(rtt_ms, 42);
                    assert_eq!(observed_at_unix_ms, 1_700_000_000_000);
                }
                other => panic!("unexpected event: {other:?}"),
            }
        })
        .await;
}
```

If `test_helpers::make_engine()` does not exist, locate the equivalent helper used by other tests in the module (e.g., one of the `tests::helpers` constructions) and adapt the call.

- [ ] **Step 3.3: Run the test**

Run: `nix develop --command cargo test -p sunset-sync pong_observed_inbound_event_propagates -- --nocapture 2>&1 | tail -20`
Expected: PASS.

- [ ] **Step 3.4: Commit**

```bash
git add crates/sunset-sync/src/engine.rs
git commit -m "$(cat <<'EOF'
sunset-sync: engine re-emits PongObserved as EngineEvent

Per-peer task → InboundEvent::PongObserved → handle_inbound_event
forwards to all engine-event subscribers (PeerSupervisor consumes
it in the next change).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 4 — Supervisor `IntentEntry` and `IntentSnapshot` gain liveness fields

**Files:**
- Modify: `crates/sunset-sync/src/supervisor.rs` (struct defs around lines 67-92, `broadcast` around 186, `Snapshot` builder around 407)

- [ ] **Step 4.1: Add fields to `IntentEntry` and `IntentSnapshot`**

In `crates/sunset-sync/src/supervisor.rs`, replace the `IntentSnapshot` and `IntentEntry` structs:

```rust
#[derive(Clone, Debug)]
pub struct IntentSnapshot {
    pub addr: PeerAddr,
    pub state: IntentState,
    pub peer_id: Option<PeerId>,
    pub attempt: u32,
    /// Wall-clock ms of the most recent Pong observed from this peer.
    /// `None` until the first Pong of the *first* connection lands.
    /// Preserved across Backoff transitions (the popover should show
    /// "heard from 12s ago" while reconnecting), cleared only on
    /// `SupervisorCommand::Remove`.
    pub last_pong_at_unix_ms: Option<u64>,
    /// Round-trip time of the most recent Pong, in milliseconds.
    /// `None` under the same conditions as `last_pong_at_unix_ms`.
    pub last_rtt_ms: Option<u64>,
}

pub(crate) struct IntentEntry {
    pub state: IntentState,
    pub attempt: u32,
    pub peer_id: Option<PeerId>,
    /// Earliest moment the next dial attempt may run. None when not in Backoff.
    pub next_attempt_at: Option<web_time::SystemTime>,
    pub last_pong_at_unix_ms: Option<u64>,
    pub last_rtt_ms: Option<u64>,
}
```

- [ ] **Step 4.2: Update every `IntentEntry { ... }` literal to include the new fields**

Two construction sites: the `SupervisorCommand::Add` handler (around line 327) and any test fixtures that build entries by hand. Add `last_pong_at_unix_ms: None, last_rtt_ms: None` to each. Search:

```
nix develop --command rg -n 'IntentEntry \{' crates/sunset-sync/src/supervisor.rs
```

Update every match.

- [ ] **Step 4.3: Update `broadcast` to emit the new fields**

Replace the body of `broadcast` (around line 186-197):

```rust
    fn broadcast(state: &mut SupervisorState, addr: &PeerAddr) {
        let Some(entry) = state.intents.get(addr) else {
            return;
        };
        let snap = IntentSnapshot {
            addr: addr.clone(),
            state: entry.state,
            peer_id: entry.peer_id.clone(),
            attempt: entry.attempt,
            last_pong_at_unix_ms: entry.last_pong_at_unix_ms,
            last_rtt_ms: entry.last_rtt_ms,
        };
        state.subscribers.retain(|tx| tx.send(snap.clone()).is_ok());
    }
```

- [ ] **Step 4.4: Update the `Snapshot` command builder**

Replace the `.map(...)` closure in the `SupervisorCommand::Snapshot` arm (around line 412):

```rust
                    .map(|(addr, e)| IntentSnapshot {
                        addr: addr.clone(),
                        state: e.state,
                        peer_id: e.peer_id.clone(),
                        attempt: e.attempt,
                        last_pong_at_unix_ms: e.last_pong_at_unix_ms,
                        last_rtt_ms: e.last_rtt_ms,
                    })
```

- [ ] **Step 4.5: Compile-check**

Run: `nix develop --command cargo build -p sunset-sync --tests 2>&1 | tail -15`
Expected: builds. Existing tests that destructure `IntentSnapshot` may need `..` added — fix as encountered.

- [ ] **Step 4.6: Commit**

```bash
git add crates/sunset-sync/src/supervisor.rs
git commit -m "$(cat <<'EOF'
sunset-sync: surface last_pong_at and last_rtt on IntentSnapshot

Adds two Option<u64> fields. None until the first Pong; preserved
across Backoff transitions. No producer yet — the PongObserved arm
lands in the next change.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 5 — Supervisor consumes `EngineEvent::PongObserved`

**Files:**
- Modify: `crates/sunset-sync/src/supervisor.rs` (`handle_engine_event`)

- [ ] **Step 5.1: Write a failing supervisor unit test**

Append to the `#[cfg(test)] mod tests` block in `crates/sunset-sync/src/supervisor.rs`:

```rust
#[tokio::test(flavor = "current_thread")]
async fn pong_observed_updates_intent_snapshot() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            // Stand up alice ↔ bob through TestTransport, add bob via
            // supervisor, wait for Connected, then drive a synthetic
            // PongObserved engine event and assert it lands on the
            // IntentSnapshot subscriber.
            let (alice, bob, bob_addr) = test_helpers::two_peer_setup().await;
            let sup = PeerSupervisor::new(alice.clone(), BackoffPolicy::default());
            crate::spawn::spawn_local({
                let s = sup.clone();
                async move { s.run().await }
            });
            sup.add(bob_addr.clone()).await.expect("add bob");
            // Drive a PongObserved on the engine; supervisor must fold
            // it into IntentEntry and broadcast.
            let bob_pid = bob.local_peer_id();
            alice
                .emit_engine_event_for_test(crate::engine::EngineEvent::PongObserved {
                    peer_id: bob_pid.clone(),
                    rtt_ms: 17,
                    observed_at_unix_ms: 1_700_000_000_500,
                })
                .await;
            // Snapshot must show the new fields.
            let snaps = sup.snapshot().await;
            let snap = snaps.iter().find(|s| s.addr == bob_addr).expect("intent");
            assert_eq!(snap.last_rtt_ms, Some(17));
            assert_eq!(snap.last_pong_at_unix_ms, Some(1_700_000_000_500));
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn pong_observed_for_unknown_peer_is_dropped() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (alice, _bob, _bob_addr) = test_helpers::two_peer_setup().await;
            let sup = PeerSupervisor::new(alice.clone(), BackoffPolicy::default());
            crate::spawn::spawn_local({
                let s = sup.clone();
                async move { s.run().await }
            });
            // Subscribe so we can observe (or not observe) broadcasts.
            let mut sub = sup.subscribe();
            let stranger = PeerId(sunset_store::VerifyingKey::new(
                bytes::Bytes::from_static(&[99u8; 32]),
            ));
            alice
                .emit_engine_event_for_test(crate::engine::EngineEvent::PongObserved {
                    peer_id: stranger,
                    rtt_ms: 1,
                    observed_at_unix_ms: 1,
                })
                .await;
            // No broadcast expected within a short window.
            use futures::StreamExt as _;
            let r = tokio::time::timeout(std::time::Duration::from_millis(100), sub.next()).await;
            assert!(r.is_err(), "expected no broadcast for unknown peer");
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn disconnect_preserves_last_pong_and_rtt() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (alice, bob, bob_addr) = test_helpers::two_peer_setup().await;
            let sup = PeerSupervisor::new(alice.clone(), BackoffPolicy::default());
            crate::spawn::spawn_local({
                let s = sup.clone();
                async move { s.run().await }
            });
            sup.add(bob_addr.clone()).await.expect("add");
            let bob_pid = bob.local_peer_id();
            alice
                .emit_engine_event_for_test(crate::engine::EngineEvent::PongObserved {
                    peer_id: bob_pid.clone(),
                    rtt_ms: 11,
                    observed_at_unix_ms: 5_000,
                })
                .await;
            // Force a disconnect (simulates network blip).
            alice.remove_peer(bob_pid.clone()).await.ok();
            // Wait for the PeerRemoved engine event to fold into Backoff.
            for _ in 0..50 {
                tokio::task::yield_now().await;
            }
            let snap = sup.snapshot().await
                .into_iter()
                .find(|s| s.addr == bob_addr)
                .expect("intent");
            assert_eq!(snap.state, IntentState::Backoff);
            assert_eq!(snap.last_rtt_ms, Some(11));
            assert_eq!(snap.last_pong_at_unix_ms, Some(5_000));
        })
        .await;
}
```

The helpers `test_helpers::two_peer_setup`, `Engine::emit_engine_event_for_test`, and `Engine::local_peer_id` may not exist verbatim. Locate the closest existing test fixture in `supervisor.rs` (e.g., `subscribe_emits_state_transitions` around line 670) and adapt names. If `emit_engine_event_for_test` is missing, add a `pub(crate) async fn emit_engine_event_for_test(...)` shim that calls the existing private `emit_engine_event` — gated `#[cfg(test)]`.

- [ ] **Step 5.2: Run tests — expect failures**

Run: `nix develop --command cargo test -p sunset-sync supervisor::tests::pong_observed -- --nocapture 2>&1 | tail -20`
Expected: the first two FAIL ("expected no broadcast" passes trivially today because PongObserved is never handled; `pong_observed_updates_intent_snapshot` FAILs because last_rtt_ms is still None). `disconnect_preserves_last_pong_and_rtt` FAILs for the same reason.

- [ ] **Step 5.3: Add the PongObserved arm in `handle_engine_event`**

In `crates/sunset-sync/src/supervisor.rs`, within `handle_engine_event` (around line 265), add a third arm:

```rust
            EngineEvent::PongObserved { peer_id, rtt_ms, observed_at_unix_ms } => {
                let mut state = self.state.borrow_mut();
                let addr = match state.peer_to_addr.get(&peer_id) {
                    Some(a) => a.clone(),
                    None => return,
                };
                if let Some(entry) = state.intents.get_mut(&addr) {
                    entry.last_pong_at_unix_ms = Some(observed_at_unix_ms);
                    entry.last_rtt_ms = Some(rtt_ms);
                }
                Self::broadcast(&mut state, &addr);
            }
```

- [ ] **Step 5.4: Verify `PeerRemoved` does NOT clear the liveness fields**

Re-read the `EngineEvent::PeerRemoved` arm (around lines 286-306). It must mutate `state` and `peer_id` only; it must NOT touch `last_pong_at_unix_ms` or `last_rtt_ms`. If it does, fix it.

- [ ] **Step 5.5: Verify `SupervisorCommand::Remove` DOES clear the fields (cleanup-on-remove semantics)**

The handler removes the entry entirely (`state.intents.remove(&addr)`), so the fields go away with it. No code change required, but confirm by re-reading lines 384-405.

- [ ] **Step 5.6: Run the tests — expect PASS**

Run: `nix develop --command cargo test -p sunset-sync supervisor::tests:: -- --nocapture 2>&1 | tail -30`
Expected: all three new tests PASS, no regressions in existing supervisor tests.

- [ ] **Step 5.7: Run the full sunset-sync test suite**

Run: `nix develop --command cargo test -p sunset-sync 2>&1 | tail -10`
Expected: all tests PASS.

- [ ] **Step 5.8: Commit**

```bash
git add crates/sunset-sync/src/supervisor.rs
git commit -m "$(cat <<'EOF'
sunset-sync: supervisor folds PongObserved into IntentSnapshot

handle_engine_event grows a PongObserved arm that updates the
IntentEntry's last_pong_at_unix_ms / last_rtt_ms and broadcasts the
new snapshot. Stale events (peer not in peer_to_addr) are dropped
silently. Disconnect preserves the fields; only Remove clears them.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 6 — wasm bridge: `intent_snapshot_to_js` includes liveness, `Client::new` accepts heartbeat override

**Files:**
- Modify: `crates/sunset-web-wasm/src/client.rs:62-130` (constructor) and `:243-270` (intent_snapshot_to_js)

- [ ] **Step 6.1: Update `Client::new` to accept `heartbeat_interval_ms`**

In `crates/sunset-web-wasm/src/client.rs:62-64`, change the signature:

```rust
    #[wasm_bindgen(constructor)]
    pub fn new(seed: &[u8], heartbeat_interval_ms: u32) -> Result<Client, JsError> {
```

Inside the body, after `Ed25519Verifier` is wired and before `SyncEngine::new`, build the config:

```rust
        let mut config = SyncConfig::default();
        if heartbeat_interval_ms > 0 {
            let interval = std::time::Duration::from_millis(heartbeat_interval_ms as u64);
            config.heartbeat_interval = interval;
            // Match default 3× ratio between interval and timeout.
            config.heartbeat_timeout = interval * 3;
        }
        let engine = Rc::new(SyncEngine::new(
            store.clone(),
            multi,
            config,
            local_peer,
            signer,
        ));
```

(Replace the existing `SyncConfig::default()` argument inline.)

- [ ] **Step 6.2: Enrich `intent_snapshot_to_js`**

In `crates/sunset-web-wasm/src/client.rs:243-270`, append two `Reflect::set` blocks before the final `Ok(obj.into())`:

```rust
    if let Some(t) = snap.last_pong_at_unix_ms {
        js_sys::Reflect::set(
            &obj,
            &JsValue::from_str("last_pong_at_unix_ms"),
            &JsValue::from_f64(t as f64),
        )
        .map_err(|_| JsError::new("Reflect::set last_pong_at_unix_ms failed"))?;
    }
    if let Some(r) = snap.last_rtt_ms {
        js_sys::Reflect::set(
            &obj,
            &JsValue::from_str("last_rtt_ms"),
            &JsValue::from_f64(r as f64),
        )
        .map_err(|_| JsError::new("Reflect::set last_rtt_ms failed"))?;
    }
```

(`u64 → f64` is safe for ms values in any plausible range; ms-since-1970 fits easily.)

- [ ] **Step 6.3: Build the wasm crate**

Run: `nix develop --command cargo build -p sunset-web-wasm --target wasm32-unknown-unknown 2>&1 | tail -10`
Expected: builds. If `cargo build` warns/errors about wasm-bindgen u32 ABI, double-check the constructor uses `u32` (not `u64` — `u64` requires bigint which the JS shim would have to format).

- [ ] **Step 6.4: Update the dev/build harness if needed**

Run the existing project bundler script and confirm it still produces `web/build/sunset_web_wasm.js` and `_bg.wasm`. Look for any helper script:

```
nix develop --command rg -n 'wasm-pack|cargo build.*wasm32|sunset_web_wasm' --type-not target | head -20
```

If `wasm-pack` is invoked from a script, run that script to refresh artefacts. Do not edit JS shims yet — Task 7 does that.

- [ ] **Step 6.5: Commit**

```bash
git add crates/sunset-web-wasm/src/client.rs
git commit -m "$(cat <<'EOF'
sunset-web-wasm: optional heartbeat override + liveness on intent JS

Client::new gains a u32 heartbeat_interval_ms (0 = use the SyncConfig
default of 15 s), mirroring presence_interval-style URL-tunability.
intent_snapshot_to_js writes last_pong_at_unix_ms and last_rtt_ms
when the supervisor has them.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 7 — Add Gleam FFI shims for relay subscription and URL param

**Files:**
- Modify: `web/src/sunset_web/sunset.ffi.mjs` (add functions)
- Modify: `web/src/sunset_web/sunset.gleam` (add externals)

- [ ] **Step 7.1: Update `createClient` shim to pass heartbeat**

In `web/src/sunset_web/sunset.ffi.mjs:47-57`, modify:

```javascript
export async function createClient(seed, heartbeatIntervalMs, callback) {
  await ensureLoaded();
  const seedBytes = bitsToBytes(seed);
  const hb = Number.isFinite(heartbeatIntervalMs) && heartbeatIntervalMs > 0
    ? heartbeatIntervalMs
    : 0;
  const client = new Client(seedBytes, hb);
  if (typeof window !== "undefined" && window.SUNSET_TEST) {
    window.sunsetClient = client;
  }
  callback(client);
}
```

- [ ] **Step 7.2: Add `heartbeatIntervalMsFromUrl` shim**

Append to `web/src/sunset_web/sunset.ffi.mjs`:

```javascript
/// Read `?heartbeat_interval_ms=NNN` from the current URL. Returns 0
/// when absent or unparseable, signalling Client::new to use the
/// SyncConfig default (15 s).
export function heartbeatIntervalMsFromUrl() {
  const params = new URLSearchParams(window.location.search);
  const raw = params.get("heartbeat_interval_ms");
  if (raw === null) return 0;
  const n = Number(raw);
  return Number.isFinite(n) && n > 0 ? n : 0;
}
```

- [ ] **Step 7.3: Add `peerConnectionSnapshot` and `subscribePeerConnections` shims**

Append to `web/src/sunset_web/sunset.ffi.mjs`:

```javascript
/// Snapshot every current peer-connection intent (relays AND direct
/// peers — caller filters by addr scheme). Returns a Promise of an
/// Array of plain JS objects:
///   { addr, state, attempt, peer_id?, last_pong_at_unix_ms?, last_rtt_ms? }
export function peerConnectionSnapshot(client, callback) {
  client
    .peer_connection_snapshot()
    .then((arr) => callback(toList(Array.from(arr))))
    .catch((e) => {
      console.warn("peerConnectionSnapshot failed", e);
      callback(toList([]));
    });
}

/// Register a callback for live peer-connection state transitions.
/// Replaces any previous callback. Each invocation receives one
/// snapshot object (same shape as peerConnectionSnapshot's elements).
export function subscribePeerConnections(client, callback) {
  client.on_peer_connection_state((snap) => {
    try {
      callback(snap);
    } catch (e) {
      console.warn("subscribePeerConnections callback threw", e);
    }
  });
}

// Per-snapshot accessors. `snap` is the plain object emitted by the
// wasm side via Reflect::set; presence/absence is meaningful.
export function snapAddr(snap) { return snap.addr; }
export function snapState(snap) { return snap.state; }
export function snapAttempt(snap) { return snap.attempt; }
export function snapPeerIdHex(snap) {
  const pk = snap.peer_id;
  if (!pk) return new GError(undefined);
  return new Ok(
    Array.from(pk, (b) => b.toString(16).padStart(2, "0")).join(""),
  );
}
export function snapLastPongAtMs(snap) {
  const v = snap.last_pong_at_unix_ms;
  return typeof v === "number" ? new Some(v) : new None();
}
export function snapLastRttMs(snap) {
  const v = snap.last_rtt_ms;
  return typeof v === "number" ? new Some(v) : new None();
}
```

- [ ] **Step 7.4: Add Gleam externals**

In `web/src/sunset_web/sunset.gleam`, add (place near other client-level externals):

```gleam
import gleam/option

@external(javascript, "./sunset.ffi.mjs", "heartbeatIntervalMsFromUrl")
pub fn heartbeat_interval_ms_from_url() -> Int

/// Opaque handle to a single JS-side intent snapshot. Always read via
/// the snap_* accessors below — never carry the raw value across an
/// async boundary, since the underlying wasm-bindgen wrapper may be
/// freed.
pub type IntentSnapshotJs

@external(javascript, "./sunset.ffi.mjs", "peerConnectionSnapshot")
pub fn peer_connection_snapshot(
  client: ClientHandle,
  callback: fn(List(IntentSnapshotJs)) -> Nil,
) -> Nil

@external(javascript, "./sunset.ffi.mjs", "subscribePeerConnections")
pub fn subscribe_peer_connections(
  client: ClientHandle,
  callback: fn(IntentSnapshotJs) -> Nil,
) -> Nil

@external(javascript, "./sunset.ffi.mjs", "snapAddr")
pub fn snap_addr(snap: IntentSnapshotJs) -> String

@external(javascript, "./sunset.ffi.mjs", "snapState")
pub fn snap_state(snap: IntentSnapshotJs) -> String

@external(javascript, "./sunset.ffi.mjs", "snapAttempt")
pub fn snap_attempt(snap: IntentSnapshotJs) -> Int

@external(javascript, "./sunset.ffi.mjs", "snapPeerIdHex")
pub fn snap_peer_id_hex(snap: IntentSnapshotJs) -> Result(String, Nil)

@external(javascript, "./sunset.ffi.mjs", "snapLastPongAtMs")
pub fn snap_last_pong_at_ms(snap: IntentSnapshotJs) -> option.Option(Int)

@external(javascript, "./sunset.ffi.mjs", "snapLastRttMs")
pub fn snap_last_rtt_ms(snap: IntentSnapshotJs) -> option.Option(Int)
```

Update the `create_client` external to include the heartbeat parameter:

```gleam
@external(javascript, "./sunset.ffi.mjs", "createClient")
pub fn create_client(
  seed: BitArray,
  heartbeat_interval_ms: Int,
  callback: fn(ClientHandle) -> Nil,
) -> Nil
```

- [ ] **Step 7.5: Compile-check**

Run: `cd web && nix develop --command gleam build 2>&1 | tail -15`
Expected: build fails because `sunset_web.gleam` still calls `sunset.create_client(seed, callback)` without the new arg. We fix that in Task 9.

- [ ] **Step 7.6: Commit**

```bash
git add web/src/sunset_web/sunset.ffi.mjs web/src/sunset_web/sunset.gleam
git commit -m "$(cat <<'EOF'
web: FFI shims for relay subscription and heartbeat URL param

Adds peerConnectionSnapshot, subscribePeerConnections, and per-snap
accessors (addr/state/attempt/peer_id_hex/last_pong_at_ms/last_rtt_ms)
plus a heartbeatIntervalMsFromUrl reader and an extra createClient
parameter. No callers wired yet; the model + view land in the next
changes.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 8 — Domain types, fixture cleanup, and removal of Bridge

**Files:**
- Modify: `web/src/sunset_web/domain.gleam`
- Modify: `web/src/sunset_web/fixture.gleam`
- Modify: `web/src/sunset_web/views/main_panel.gleam`
- Modify: `web/src/sunset_web/views/channels.gleam`

- [ ] **Step 8.1: Add new domain types and remove Bridge**

In `web/src/sunset_web/domain.gleam`, replace the `ChannelKind`, `BridgeKind`, and `BridgeOpt` definitions plus the `bridge:` field on `Room`/`Member`/`Message`:

Delete:
```gleam
pub type BridgeKind {
  Minecraft
}
pub type BridgeOpt {
  HasBridge(BridgeKind)
  NoBridge
}
```

Change `ChannelKind` from:
```gleam
pub type ChannelKind {
  TextChannel
  Voice
  Bridge(BridgeKind)
}
```
to:
```gleam
pub type ChannelKind {
  TextChannel
  Voice
}
```

Remove `bridge: BridgeOpt,` from `Room`, `Member`, and `Message`.

Append the new types:

```gleam
pub type RelayConnState {
  RelayConnecting
  RelayConnected
  RelayBackoff
  RelayCancelled
}

pub type Relay {
  Relay(
    /// Raw addr URL (the supervisor's identity for an intent).
    addr: String,
    /// Display label parsed from `addr`. Best-effort — falls back
    /// to `addr` when the URL is unparseable.
    host: String,
    state: RelayConnState,
    attempt: Int,
    /// Short pubkey (first 4 + last 4 hex bytes) once the Noise
    /// handshake completes. `None` while connecting.
    peer_id_short: option.Option(String),
    /// Wall-clock ms of the most recent Pong from this relay.
    /// `None` until the first Pong of the first connection lands.
    last_pong_at_ms: option.Option(Int),
    /// Round-trip time of the most recent Pong, in milliseconds.
    last_rtt_ms: option.Option(Int),
  )
}
```

- [ ] **Step 8.2: Strip Bridge usage from `fixture.gleam`**

In `web/src/sunset_web/fixture.gleam`, remove (a) the `Channel(... kind: Bridge(Minecraft) ...)` row whose name is `"minecraft-bridge"`; (b) every occurrence of `bridge: NoBridge,` on Room/Member/Message constructors; (c) every `bridge: HasBridge(Minecraft),` (member rows that "came from the bridge"); (d) update the import line to drop `Bridge`, `BridgeRelay`, `HasBridge`, `Minecraft`, `NoBridge`. The `Receipt(name: "elena", time: "5:49:18 pm", relay: BridgeRelay)` row also goes — substitute `relay: NoRelay` (Receipt's relay field is the `RelayStatus` enum, unrelated to the new `Relay` type, but `BridgeRelay` is being removed below).

- [ ] **Step 8.3: Drop `BridgeRelay` from `RelayStatus`**

In `web/src/sunset_web/domain.gleam`, change `RelayStatus`:

```gleam
pub type RelayStatus {
  Direct
  OneHop
  TwoHop
  ViaPeer(String)
  SelfRelay
  NoRelay
}
```

(Removed `BridgeRelay` because the Bridge concept is gone; `Receipt`'s `relay` field falls back to `NoRelay` for fixture rows that previously used it. If any non-fixture code constructs `BridgeRelay`, find and fix.)

- [ ] **Step 8.4: Remove `bridge_tag` and bridge handling from `main_panel.gleam`**

In `web/src/sunset_web/views/main_panel.gleam`:
- Remove the `HasBridge`, `Minecraft`, `NoBridge` items from the `domain.{...}` import on line 24-26.
- Remove the `case m.bridge { HasBridge(Minecraft) -> ... NoBridge -> ... }` block at line 647-649; replace with `[]`.
- Delete the `fn bridge_tag` definition at line 1025+.

- [ ] **Step 8.5: Remove bridge handling from `channels.gleam`**

In `web/src/sunset_web/views/channels.gleam`:
- Remove `Bridge`, `Minecraft` from the `domain.{...}` import on line 12-16.
- Remove the `bridge_channels` filter (lines 34-40) and the `case bridge_channels { ... } -> section(p, "Bridges", ...)` block (lines 94-102). Leave a placeholder `element.fragment([])` where the section was; Task 11 replaces it with the real Relays section.
- Delete `fn bridge_channel_row` (lines 871-894).

- [ ] **Step 8.6: Build Gleam code**

Run: `cd web && nix develop --command gleam build 2>&1 | tail -25`
Expected: still fails on `create_client` arity (Task 9 fixes), but no other errors should remain. Read each error and fix any missed `bridge:` occurrence or stale import.

- [ ] **Step 8.7: Commit**

```bash
git add web/src/sunset_web/domain.gleam web/src/sunset_web/fixture.gleam web/src/sunset_web/views/main_panel.gleam web/src/sunset_web/views/channels.gleam
git commit -m "$(cat <<'EOF'
web: remove Bridge / Minecraft fixture types; add Relay domain

The dummy minecraft-bridge channel and bridge: fields on Room /
Member / Message had no real producers; nothing in the runtime read
them. Replaced by a real Relay { addr, host, state, attempt,
peer_id_short, last_pong_at_ms, last_rtt_ms } type that the next
change will populate from the supervisor.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 9 — Pure helpers + Gleam unit tests

**Files:**
- Create: `web/src/sunset_web/views/relays.gleam` (helpers only — view comes in Task 10)
- Create: `web/test/sunset_web/views/relays_test.gleam`

- [ ] **Step 9.1: Create `relays.gleam` with the pure helpers only**

```gleam
//// Relays UI: rail-section list + click-through popover (desktop
//// floating / phone bottom sheet). This file currently exposes only
//// the pure helpers so they can be unit-tested. The view functions
//// land in the next change.

import gleam/int
import gleam/list
import gleam/option.{type Option}
import gleam/string
import gleam/uri
import sunset_web/domain.{
  type Relay, type RelayConnState, RelayBackoff, RelayCancelled, RelayConnected,
  RelayConnecting,
}

/// True for ws:// and wss:// addrs. Direct WebRTC peers (`webrtc://`)
/// are NOT relays and must be excluded from the rail.
pub fn is_relay_addr(addr: String) -> Bool {
  string.starts_with(addr, "ws://") || string.starts_with(addr, "wss://")
}

/// Best-effort hostname extraction. Returns `addr` unchanged when the
/// URL fails to parse — defensive fallback so a malformed addr never
/// crashes the rail. Includes the port when non-default
/// ("127.0.0.1:8080" → "127.0.0.1:8080").
pub fn parse_host(addr: String) -> String {
  case uri.parse(addr) {
    Ok(u) ->
      case u.host {
        option.Some(h) ->
          case u.port {
            option.Some(p) -> h <> ":" <> int.to_string(p)
            option.None -> h
          }
        option.None -> addr
      }
    Error(_) -> addr
  }
}

/// User-facing label for the connection state. For Backoff with a
/// non-zero attempt counter, includes the attempt number.
pub fn format_status(state: RelayConnState, attempt: Int) -> String {
  case state, attempt {
    RelayConnected, _ -> "Connected"
    RelayConnecting, _ -> "Connecting"
    RelayBackoff, 0 -> "Backoff"
    RelayBackoff, n -> "Backoff (attempt " <> int.to_string(n) <> ")"
    RelayCancelled, _ -> "Cancelled"
  }
}

/// "RTT 42 ms" / "RTT —".
pub fn format_rtt(last_rtt_ms: Option(Int)) -> String {
  case last_rtt_ms {
    option.Some(n) -> "RTT " <> int.to_string(n) <> " ms"
    option.None -> "RTT —"
  }
}

/// Render age "heard from …": "just now" / "Ns ago" / "Nm ago" /
/// "Nh ago" / "never". Mirrors `peer_status_popover.humanize_age`;
/// kept duplicated rather than pre-extracting a shared helper —
/// extract when a third caller appears.
pub fn humanize_age(now_ms: Int, last_ms: Option(Int)) -> String {
  case last_ms {
    option.None -> "never"
    option.Some(t) -> {
      let age_ms = case now_ms - t {
        n if n < 0 -> 0
        n -> n
      }
      let age_s = age_ms / 1000
      case age_s {
        s if s < 1 -> "just now"
        s if s < 60 -> int.to_string(s) <> "s ago"
        s if s < 3600 -> int.to_string(s / 60) <> "m ago"
        s -> int.to_string(s / 3600) <> "h ago"
      }
    }
  }
}

/// Map a JS-side intent state string to the typed enum. Unknown
/// strings fall back to `RelayBackoff` so the row stays visible in
/// some recoverable state rather than being silently dropped.
pub fn parse_state(s: String) -> RelayConnState {
  case s {
    "connected" -> RelayConnected
    "connecting" -> RelayConnecting
    "backoff" -> RelayBackoff
    "cancelled" -> RelayCancelled
    _ -> RelayBackoff
  }
}

/// Format a hex pubkey as "first8…last8" (8 chars on each side).
/// Strings of length ≤ 16 are returned unchanged.
pub fn short_peer_id(hex: String) -> String {
  case string.length(hex) {
    n if n <= 16 -> hex
    n -> string.slice(hex, 0, 8) <> "…" <> string.slice(hex, n - 8, 8)
  }
}

/// Upsert a snapshot into a list of relays keyed by `addr`. Preserves
/// existing insertion order; appends new addrs at the end.
pub fn upsert(existing: List(Relay), updated: Relay) -> List(Relay) {
  case list.find(existing, fn(r) { r.addr == updated.addr }) {
    Ok(_) ->
      list.map(existing, fn(r) {
        case r.addr == updated.addr {
          True -> updated
          False -> r
        }
      })
    Error(_) -> list.append(existing, [updated])
  }
}
```

- [ ] **Step 9.2: Create the test file**

```gleam
import gleam/option
import gleeunit/should
import sunset_web/domain.{
  Relay, RelayBackoff, RelayCancelled, RelayConnected, RelayConnecting,
}
import sunset_web/views/relays

pub fn is_relay_addr_wss_test() {
  relays.is_relay_addr("wss://relay.sunset.chat?x=1#x25519=abc")
  |> should.be_true()
}

pub fn is_relay_addr_ws_test() {
  relays.is_relay_addr("ws://127.0.0.1:8080")
  |> should.be_true()
}

pub fn is_relay_addr_webrtc_test() {
  relays.is_relay_addr("webrtc://abc#x25519=def")
  |> should.be_false()
}

pub fn is_relay_addr_https_test() {
  relays.is_relay_addr("https://example.com")
  |> should.be_false()
}

pub fn parse_host_simple_test() {
  relays.parse_host("wss://relay.sunset.chat")
  |> should.equal("relay.sunset.chat")
}

pub fn parse_host_with_port_test() {
  relays.parse_host("ws://127.0.0.1:8080")
  |> should.equal("127.0.0.1:8080")
}

pub fn parse_host_with_path_query_fragment_test() {
  relays.parse_host("wss://relay.sunset.chat:443/api?token=foo#x25519=abc")
  |> should.equal("relay.sunset.chat:443")
}

pub fn parse_host_unparseable_falls_back_test() {
  relays.parse_host("not a url at all")
  |> should.equal("not a url at all")
}

pub fn format_status_connected_test() {
  relays.format_status(RelayConnected, 0)
  |> should.equal("Connected")
}

pub fn format_status_connecting_test() {
  relays.format_status(RelayConnecting, 0)
  |> should.equal("Connecting")
}

pub fn format_status_backoff_zero_test() {
  relays.format_status(RelayBackoff, 0)
  |> should.equal("Backoff")
}

pub fn format_status_backoff_with_attempt_test() {
  relays.format_status(RelayBackoff, 3)
  |> should.equal("Backoff (attempt 3)")
}

pub fn format_status_cancelled_test() {
  relays.format_status(RelayCancelled, 7)
  |> should.equal("Cancelled")
}

pub fn format_rtt_present_test() {
  relays.format_rtt(option.Some(42))
  |> should.equal("RTT 42 ms")
}

pub fn format_rtt_absent_test() {
  relays.format_rtt(option.None)
  |> should.equal("RTT —")
}

pub fn humanize_age_just_now_test() {
  relays.humanize_age(1000, option.Some(800))
  |> should.equal("just now")
}

pub fn humanize_age_seconds_test() {
  relays.humanize_age(5500, option.Some(500))
  |> should.equal("5s ago")
}

pub fn humanize_age_never_test() {
  relays.humanize_age(0, option.None)
  |> should.equal("never")
}

pub fn parse_state_known_test() {
  relays.parse_state("connected") |> should.equal(RelayConnected)
  relays.parse_state("connecting") |> should.equal(RelayConnecting)
  relays.parse_state("backoff") |> should.equal(RelayBackoff)
  relays.parse_state("cancelled") |> should.equal(RelayCancelled)
}

pub fn parse_state_unknown_falls_back_to_backoff_test() {
  relays.parse_state("eldritch_state")
  |> should.equal(RelayBackoff)
}

pub fn short_peer_id_short_unchanged_test() {
  relays.short_peer_id("abcdef")
  |> should.equal("abcdef")
}

pub fn short_peer_id_truncates_test() {
  relays.short_peer_id("0123456789abcdef0123456789abcdef")
  |> should.equal("01234567…89abcdef")
}

pub fn upsert_inserts_new_at_end_test() {
  let r =
    Relay(
      addr: "wss://a",
      host: "a",
      state: RelayConnecting,
      attempt: 0,
      peer_id_short: option.None,
      last_pong_at_ms: option.None,
      last_rtt_ms: option.None,
    )
  relays.upsert([], r)
  |> should.equal([r])
}

pub fn upsert_replaces_existing_test() {
  let r1 =
    Relay(
      addr: "wss://a",
      host: "a",
      state: RelayConnecting,
      attempt: 0,
      peer_id_short: option.None,
      last_pong_at_ms: option.None,
      last_rtt_ms: option.None,
    )
  let r2 = Relay(..r1, state: RelayConnected, last_rtt_ms: option.Some(42))
  let r3 = Relay(..r1, addr: "wss://b", host: "b")
  relays.upsert([r1, r3], r2)
  |> should.equal([r2, r3])
}
```

- [ ] **Step 9.3: Run the Gleam tests**

Run: `cd web && nix develop --command gleam test 2>&1 | tail -25`
Expected: tests pass. (The full app `gleam build` may still fail at this point because `sunset_web.gleam`'s `create_client` call is stale; `gleam test` runs the test target separately and should still build the unit-test code paths if they don't transitively pull in the broken main module. If `gleam test` *also* fails on main module compile, complete Task 10 first and circle back.)

- [ ] **Step 9.4: Commit**

```bash
git add web/src/sunset_web/views/relays.gleam web/test/sunset_web/views/relays_test.gleam
git commit -m "$(cat <<'EOF'
web: relays.gleam pure helpers + unit tests

is_relay_addr / parse_host (gleam/uri-backed, with addr fallback) /
format_status (with attempt-N rendering for Backoff) / format_rtt /
humanize_age (mirrors peer_status_popover; kept duplicated per spec) /
parse_state / short_peer_id / upsert. View entry points come in the
next change.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 10 — Wire model + subscription + popover state in `sunset_web.gleam`

**Files:**
- Modify: `web/src/sunset_web.gleam` (Model, Msg, init effect, update branches, view wiring)

- [ ] **Step 10.1: Add `relays` and `relays_popover` to the Model**

In `web/src/sunset_web.gleam`, locate the `Model(...)` record (around line 145) and add:

```gleam
    /// All relay intents currently registered with the supervisor.
    /// Updated from on_peer_connection_state. Direct WebRTC peers are
    /// excluded by `relays.is_relay_addr` before insertion.
    relays: List(domain.Relay),
    /// Address of the relay whose popover is open, if any. Phone +
    /// desktop share this state — placement is decided by `viewport`.
    relays_popover: option.Option(String),
```

In the `Model(..., relay_status: "disconnected")` initialiser (around line 284), add:

```gleam
      relays: [],
      relays_popover: option.None,
```

- [ ] **Step 10.2: Add new Msg variants**

In the `pub type Msg { ... }` block (around line 200-220), add:

```gleam
  PeerConnectionSnapshotSeed(List(sunset.IntentSnapshotJs))
  PeerConnectionStateUpdated(sunset.IntentSnapshotJs)
  OpenRelayPopover(String)
  CloseRelayPopover
```

- [ ] **Step 10.3: Pass the heartbeat URL param into `create_client`**

Locate the `sunset.create_client(seed, fn(client) { ... })` call site (the only one — search `rg -n 'create_client' web/src/`). Change to:

```gleam
let hb = sunset.heartbeat_interval_ms_from_url()
sunset.create_client(seed, hb, fn(client) {
  ...
})
```

- [ ] **Step 10.4: Subscribe at Client creation time**

Inside the `create_client` callback (right after the `ClientHandle` is captured, before any `add_relay` is fired — search for where `add_relay` is called from the bootstrap effect, around line 740-748), add:

```gleam
sunset.peer_connection_snapshot(client, fn(snaps) {
  dispatch(PeerConnectionSnapshotSeed(snaps))
})
sunset.subscribe_peer_connections(client, fn(snap) {
  dispatch(PeerConnectionStateUpdated(snap))
})
```

- [ ] **Step 10.5: Add a helper that maps a JS snapshot to a `domain.Relay`**

Above the `update` function (or near the bottom of the file with other helpers), add:

```gleam
fn snap_to_relay(snap: sunset.IntentSnapshotJs) -> domain.Relay {
  let addr = sunset.snap_addr(snap)
  let state = relays_view.parse_state(sunset.snap_state(snap))
  let peer_id_short =
    case sunset.snap_peer_id_hex(snap) {
      Ok(hex) -> option.Some(relays_view.short_peer_id(hex))
      Error(_) -> option.None
    }
  domain.Relay(
    addr: addr,
    host: relays_view.parse_host(addr),
    state: state,
    attempt: sunset.snap_attempt(snap),
    peer_id_short: peer_id_short,
    last_pong_at_ms: sunset.snap_last_pong_at_ms(snap),
    last_rtt_ms: sunset.snap_last_rtt_ms(snap),
  )
}
```

Add a corresponding alias near the imports:

```gleam
import sunset_web/views/relays as relays_view
```

- [ ] **Step 10.6: Handle the new Msgs in `update`**

In the `pub fn update` arms, add:

```gleam
    PeerConnectionSnapshotSeed(snaps) -> {
      let new_relays =
        snaps
        |> list.filter(fn(s) { relays_view.is_relay_addr(sunset.snap_addr(s)) })
        |> list.map(snap_to_relay)
      #(Model(..model, relays: new_relays), effect.none())
    }
    PeerConnectionStateUpdated(snap) -> {
      let addr = sunset.snap_addr(snap)
      case relays_view.is_relay_addr(addr) {
        False -> #(model, effect.none())
        True -> {
          let r = snap_to_relay(snap)
          #(Model(..model, relays: relays_view.upsert(model.relays, r)), effect.none())
        }
      }
    }
    OpenRelayPopover(addr) -> #(
      Model(..model, relays_popover: option.Some(addr)),
      effect.none(),
    )
    CloseRelayPopover -> #(
      Model(..model, relays_popover: option.None),
      effect.none(),
    )
```

- [ ] **Step 10.7: Compile-check**

Run: `cd web && nix develop --command gleam build 2>&1 | tail -25`
Expected: builds, but the channels rail still doesn't render relays (Task 11) and the popover overlay still isn't mounted (Task 12).

- [ ] **Step 10.8: Commit**

```bash
git add web/src/sunset_web.gleam
git commit -m "$(cat <<'EOF'
web: subscribe to peer-connection state and seed Relays in Model

create_client now passes the URL-tunable heartbeat_interval_ms and
immediately wires peer_connection_snapshot + subscribe_peer_connections
so the supervisor's IntentSnapshot stream populates Model.relays.
Direct peers (webrtc://) are filtered out.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 11 — Render the Relays section in the channels rail

**Files:**
- Modify: `web/src/sunset_web/views/relays.gleam` (add `rail_section` view)
- Modify: `web/src/sunset_web/views/channels.gleam` (replace the placeholder slot from Task 8.5)
- Modify: `web/src/sunset_web.gleam` (pass `relays` and `OpenRelayPopover` into `channels.view`)

- [ ] **Step 11.1: Add `rail_section` to `relays.gleam`**

Add the following imports at the top of `web/src/sunset_web/views/relays.gleam` (Gleam requires imports at the top — append the function definitions at the bottom but the imports go up with the existing `import` block):

```gleam
import lustre/attribute
import lustre/element.{type Element}
import lustre/element/html
import lustre/event
import sunset_web/theme.{type Palette}
import sunset_web/ui
```

Then append the function definitions to the end of the file:

```gleam

pub fn rail_section(
  palette p: Palette,
  relays rs: List(domain.Relay),
  on_open on_open: fn(String) -> msg,
) -> Element(msg) {
  case rs {
    [] -> element.fragment([])
    _ ->
      html.div(
        [
          attribute.attribute("data-testid", "relays-section"),
          ui.css([
            #("display", "flex"),
            #("flex-direction", "column"),
            #("gap", "4px"),
          ]),
        ],
        [
          html.div(
            [
              ui.css([
                #("padding", "0 12px 4px 12px"),
                #("font-size", "13.125px"),
                #("color", p.text_faint),
                #("text-transform", "uppercase"),
                #("letter-spacing", "0.04em"),
              ]),
            ],
            [html.text("Relays")],
          ),
          html.div(
            [
              ui.css([
                #("display", "flex"),
                #("flex-direction", "column"),
                #("gap", "1px"),
              ]),
            ],
            list.map(rs, fn(r) { rail_row(p, r, on_open) }),
          ),
        ],
      )
  }
}

fn rail_row(
  p: Palette,
  r: domain.Relay,
  on_open: fn(String) -> msg,
) -> Element(msg) {
  html.button(
    [
      attribute.attribute("data-testid", "relay-row"),
      attribute.attribute("data-relay-host", r.host),
      attribute.attribute("data-relay-state", state_attr(r.state)),
      event.on_click(on_open(r.addr)),
      ui.css([
        #("display", "flex"),
        #("align-items", "center"),
        #("gap", "8px"),
        #("padding", "6px 12px"),
        #("border", "none"),
        #("background", "transparent"),
        #("color", p.text_muted),
        #("font-family", "inherit"),
        #("font-size", "16.25px"),
        #("text-align", "left"),
        #("cursor", "pointer"),
        #("border-radius", "6px"),
      ]),
    ],
    [
      conn_dot(p, r.state),
      html.span([ui.css([#("flex", "1"), #("min-width", "0"),
        #("white-space", "nowrap"), #("overflow", "hidden"),
        #("text-overflow", "ellipsis")])], [html.text(r.host)]),
    ],
  )
}

fn conn_dot(p: Palette, s: domain.RelayConnState) -> Element(msg) {
  let c = case s {
    domain.RelayConnected -> p.live
    domain.RelayConnecting -> p.warn
    domain.RelayBackoff -> p.warn
    domain.RelayCancelled -> p.text_faint
  }
  html.span(
    [
      ui.css([
        #("width", "7px"),
        #("height", "7px"),
        #("border-radius", "999px"),
        #("background", c),
        #("display", "inline-block"),
        #("flex-shrink", "0"),
      ]),
    ],
    [],
  )
}

fn state_attr(s: domain.RelayConnState) -> String {
  case s {
    domain.RelayConnected -> "connected"
    domain.RelayConnecting -> "connecting"
    domain.RelayBackoff -> "backoff"
    domain.RelayCancelled -> "cancelled"
  }
}
```

- [ ] **Step 11.2: Wire `rail_section` into `channels.gleam`**

In `web/src/sunset_web/views/channels.gleam`, add to the `view` signature (alongside `voice_popover_open`, etc.):

```gleam
  relays relays: List(domain.Relay),
  on_open_relay on_open_relay: fn(String) -> msg,
```

Add the import:

```gleam
import sunset_web/views/relays as relays_view
```

Replace the placeholder fragment that previously sat where the Bridges section was, with:

```gleam
          relays_view.rail_section(
            palette: p,
            relays: relays,
            on_open: on_open_relay,
          ),
```

- [ ] **Step 11.3: Pass relays from the shell**

In `web/src/sunset_web.gleam`, locate the `channels.view(...)` call site (around line 1308 area where channels rail mounts) and add:

```gleam
        relays: model.relays,
        on_open_relay: OpenRelayPopover,
```

If `channels.view` is invoked from multiple branches (desktop / phone drawer), update each.

- [ ] **Step 11.4: Build and visually verify locally (no popover yet)**

Run: `cd web && nix develop --command gleam build 2>&1 | tail -10`
Expected: clean.

Run the dev server: `cd web && nix develop --command npm run dev` (or whatever is in `web/package.json`'s scripts). Open in a browser, confirm the "Relays" section appears under the channels rail when a relay is configured. Click does nothing yet — popover lands in Task 12. (For UI changes, the CLAUDE.md rule says to verify in a browser before claiming complete; do this here even though the popover is not finished.)

- [ ] **Step 11.5: Commit**

```bash
git add web/src/sunset_web/views/relays.gleam web/src/sunset_web/views/channels.gleam web/src/sunset_web.gleam
git commit -m "$(cat <<'EOF'
web: render Relays section in the channels rail

Replaces the dummy "Bridges" slot with a live list of relays sourced
from the supervisor's IntentSnapshot stream. Status dot colours mirror
ConnStatus (live / warn / faint). Click handler dispatches
OpenRelayPopover; popover wiring lands next.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 12 — Popover view + desktop floating / phone bottom-sheet wiring

**Files:**
- Modify: `web/src/sunset_web/views/relays.gleam` (add `popover` view + Placement)
- Modify: `web/src/sunset_web.gleam` (mount the overlay alongside `peer_status_popover_overlay`)

- [ ] **Step 12.1: Add the popover view**

Append to `web/src/sunset_web/views/relays.gleam`:

```gleam
pub type Placement {
  Floating
  InSheet
}

pub fn popover(
  palette p: Palette,
  relay r: domain.Relay,
  now_ms now: Int,
  placement placement: Placement,
  on_close on_close: msg,
) -> Element(msg) {
  let body =
    html.div(
      [
        ui.css([
          #("display", "flex"),
          #("flex-direction", "column"),
          #("gap", "10px"),
          #("padding", "14px 16px"),
        ]),
      ],
      [
        header(p, r.host, on_close),
        status_pill(p, r.state, format_status(r.state, r.attempt)),
        info_row(p, "relay-popover-heard-from",
          "heard from " <> humanize_age(now, r.last_pong_at_ms)),
        info_row(p, "relay-popover-rtt", format_rtt(r.last_rtt_ms)),
        mono_row(p, "relay-popover-addr", r.addr),
        case r.peer_id_short {
          option.Some(s) -> mono_row(p, "relay-popover-peer-id", s)
          option.None -> element.fragment([])
        },
      ],
    )

  case placement {
    Floating ->
      html.div(
        [
          attribute.attribute("data-testid", "relay-popover"),
          ui.css([
            #("position", "fixed"),
            #("top", "120px"),
            #("right", "260px"),
            #("width", "300px"),
            #("background", p.surface),
            #("color", p.text),
            #("border", "1px solid " <> p.border),
            #("border-radius", "10px"),
            #("box-shadow", p.shadow_lg),
            #("z-index", "20"),
          ]),
        ],
        [body],
      )
    InSheet ->
      html.div(
        [
          attribute.attribute("data-testid", "relay-popover"),
          ui.css([
            #("display", "flex"),
            #("flex-direction", "column"),
            #("background", p.surface),
            #("color", p.text),
          ]),
        ],
        [body],
      )
  }
}

fn header(p: Palette, host: String, on_close: msg) -> Element(msg) {
  html.div(
    [
      ui.css([
        #("display", "flex"),
        #("align-items", "center"),
        #("justify-content", "space-between"),
        #("gap", "8px"),
      ]),
    ],
    [
      html.span(
        [
          ui.css([
            #("font-weight", "600"),
            #("font-size", "16px"),
            #("color", p.text),
            #("white-space", "nowrap"),
            #("overflow", "hidden"),
            #("text-overflow", "ellipsis"),
          ]),
        ],
        [html.text(host)],
      ),
      html.button(
        [
          attribute.attribute("data-testid", "relay-popover-close"),
          event.on_click(on_close),
          ui.css([
            #("background", "transparent"),
            #("border", "none"),
            #("color", p.text_faint),
            #("cursor", "pointer"),
            #("font-size", "16px"),
            #("padding", "0 4px"),
          ]),
        ],
        [html.text("×")],
      ),
    ],
  )
}

fn status_pill(p: Palette, state: domain.RelayConnState, label: String) -> Element(msg) {
  let bg = case state {
    domain.RelayConnected -> p.live
    domain.RelayConnecting -> p.warn
    domain.RelayBackoff -> p.warn
    domain.RelayCancelled -> p.text_faint
  }
  html.span(
    [
      attribute.attribute("data-testid", "relay-popover-status"),
      ui.css([
        #("align-self", "flex-start"),
        #("padding", "2px 8px"),
        #("border-radius", "999px"),
        #("background", bg),
        #("color", p.accent_ink),
        #("font-size", "13px"),
        #("font-weight", "600"),
      ]),
    ],
    [html.text(label)],
  )
}

fn info_row(p: Palette, testid: String, text: String) -> Element(msg) {
  html.div(
    [
      attribute.attribute("data-testid", testid),
      ui.css([
        #("font-size", "14px"),
        #("color", p.text_muted),
      ]),
    ],
    [html.text(text)],
  )
}

fn mono_row(p: Palette, testid: String, text: String) -> Element(msg) {
  html.div(
    [
      attribute.attribute("data-testid", testid),
      ui.css([
        #("font-family", "monospace"),
        #("font-size", "13px"),
        #("color", p.text_faint),
        #("word-break", "break-all"),
      ]),
    ],
    [html.text(text)],
  )
}
```

- [ ] **Step 12.2: Mount the overlay in `sunset_web.gleam`**

Locate `peer_status_popover_overlay` (around line 1516) and the matching `peer_status_sheet_el` (around line 1375). Add a sibling for relays.

For the desktop overlay (search for `peer_status_popover_overlay(palette, model, state)` mount and add immediately after):

```gleam
relay_popover_overlay(palette, model),
```

For the phone bottom sheet (search for `peer_status_sheet_el` definition and add a `relay_sheet_el`):

```gleam
let relay_sheet_el = case model.viewport, model.relays_popover {
  domain.Phone, option.Some(addr) ->
    case list.find(model.relays, fn(r) { r.addr == addr }) {
      Ok(r) ->
        bottom_sheet.view(
          palette: palette,
          on_close: CloseRelayPopover,
          content: relays_view.popover(
            palette: palette,
            relay: r,
            now_ms: model.now_ms,
            placement: relays_view.InSheet,
            on_close: CloseRelayPopover,
          ),
        )
      Error(_) -> element.fragment([])
    }
  _, _ -> element.fragment([])
}
```

And mount `relay_sheet_el` in the same `case` arm where `peer_status_sheet_el` is mounted (under the `Phone` branch of the shell render).

Define the desktop overlay function near `peer_status_popover_overlay`:

```gleam
fn relay_popover_overlay(palette: theme.Palette, model: Model) -> Element(Msg) {
  case model.viewport, model.relays_popover {
    domain.Desktop, option.Some(addr) ->
      case list.find(model.relays, fn(r) { r.addr == addr }) {
        Ok(r) ->
          relays_view.popover(
            palette: palette,
            relay: r,
            now_ms: model.now_ms,
            placement: relays_view.Floating,
            on_close: CloseRelayPopover,
          )
        Error(_) -> element.fragment([])
      }
    _, _ -> element.fragment([])
  }
}
```

If the model field for the live "now" tick is named differently (search `rg -n 'now_ms\|NowTick\|setIntervalMs' web/src/`), substitute the actual field name.

- [ ] **Step 12.3: Build and verify in the browser**

Run: `cd web && nix develop --command gleam build 2>&1 | tail -10`
Expected: clean.

Open the dev server, click a Relays row. Desktop: a floating card opens top-right with hostname / status pill / "heard from" / RTT / addr. Phone (DevTools 390×844): the same content slides up as a bottom sheet. Close via × works in both.

- [ ] **Step 12.4: Commit**

```bash
git add web/src/sunset_web/views/relays.gleam web/src/sunset_web.gleam
git commit -m "$(cat <<'EOF'
web: relay popover (desktop floating + phone bottom sheet)

Click a Relays row to see hostname header, status pill, last
heartbeat (live-ticking via the existing now_ms ticker), RTT, full
addr URL (mono, selectable), and short peer id (when known).
Placement mirrors peer_status_popover (Floating | InSheet) and
mounts inside the existing bottom_sheet host on phone.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 13 — Playwright e2e

**Files:**
- Create: `web/e2e/relays.spec.js`

- [ ] **Step 13.1: Author the spec**

```javascript
// Acceptance test for the Relays rail and popover.
//
// Spawns a real sunset-relay, points one browser at it, and asserts:
//   * Relays section + row appear with the correct hostname.
//   * Row state attribute reaches "connected".
//   * Click opens the popover (hostname, status, heard-from, RTT, addr).
//   * Live age + RTT update once a Pong round-trips
//     (heartbeat_interval_ms=2000 keeps the test under 15 s).
//   * On phone viewport, the popover renders inside the bottom sheet.

import { test, expect } from "@playwright/test";
import { spawn } from "child_process";
import { mkdtempSync, rmSync } from "fs";
import { tmpdir } from "os";
import { join } from "path";

let relayProcess = null;
let relayAddress = null;
let relayDataDir = null;

test.beforeAll(async () => {
  relayDataDir = mkdtempSync(join(tmpdir(), "sunset-relay-relays-"));
  const configPath = join(relayDataDir, "relay.toml");
  const fs = await import("fs/promises");
  await fs.writeFile(
    configPath,
    [
      `listen_addr = "127.0.0.1:0"`,
      `data_dir = "${relayDataDir}"`,
      `interest_filter = "all"`,
      `identity_secret = "auto"`,
      `peers = []`,
      "",
    ].join("\n"),
  );
  relayProcess = spawn("sunset-relay", ["--config", configPath], {
    stdio: ["ignore", "pipe", "pipe"],
  });
  relayAddress = await new Promise((resolve, reject) => {
    const timer = setTimeout(
      () => reject(new Error("relay didn't print address banner within 15s")),
      15_000,
    );
    let buffer = "";
    relayProcess.stdout.on("data", (chunk) => {
      buffer += chunk.toString();
      const m = buffer.match(/address:\s+(ws:\/\/[^\s]+)/);
      if (m) {
        clearTimeout(timer);
        resolve(m[1]);
      }
    });
    relayProcess.stderr.on("data", (chunk) =>
      process.stderr.write(`[relay] ${chunk}`),
    );
    relayProcess.on("error", (e) => {
      clearTimeout(timer);
      reject(e);
    });
    relayProcess.on("exit", (code) => {
      if (code !== null && code !== 0) {
        clearTimeout(timer);
        reject(new Error(`relay exited prematurely (code ${code})`));
      }
    });
  });
});

test.afterAll(async () => {
  if (relayProcess && relayProcess.exitCode === null) {
    relayProcess.kill("SIGTERM");
  }
  if (relayDataDir) {
    rmSync(relayDataDir, { recursive: true, force: true });
  }
});

const buildUrl = () =>
  `/?relay=${encodeURIComponent(relayAddress)}` +
  `&heartbeat_interval_ms=2000` +
  `#sunset-relays`;

// Compute the host/port string we expect rendered, matching parse_host's
// behaviour on `ws://127.0.0.1:NNN`.
function expectedHost() {
  const u = new URL(relayAddress);
  return u.port ? `${u.hostname}:${u.port}` : u.hostname;
}

test.setTimeout(45_000);

test("desktop: relay row appears and popover shows live metrics", async ({ page }) => {
  page.on("pageerror", (err) =>
    process.stderr.write(`[pageerror] ${err.stack || err}\n`),
  );
  await page.goto(buildUrl());
  // The Relays section is hidden when empty; wait for it to materialise.
  await expect(page.locator('[data-testid="relays-section"]')).toBeVisible({
    timeout: 10_000,
  });
  const row = page.locator(
    `[data-testid="relay-row"][data-relay-host="${expectedHost()}"]`,
  );
  await expect(row).toBeVisible();
  await expect(row).toHaveAttribute("data-relay-state", "connected", {
    timeout: 10_000,
  });

  await row.click();
  const popover = page.locator('[data-testid="relay-popover"]');
  await expect(popover).toBeVisible();
  await expect(
    popover.locator('[data-testid="relay-popover-status"]'),
  ).toHaveText("Connected");
  await expect(
    popover.locator('[data-testid="relay-popover-addr"]'),
  ).toContainText(relayAddress);

  // Within ~6 s (3 × heartbeat_interval_ms=2000) we should see a
  // measured RTT and a humanised heartbeat age.
  const rtt = popover.locator('[data-testid="relay-popover-rtt"]');
  await expect(rtt).toHaveText(/^RTT \d+ ms$/, { timeout: 8_000 });
  const heard = popover.locator('[data-testid="relay-popover-heard-from"]');
  await expect(heard).toHaveText(/^heard from (just now|\d+s ago)$/, {
    timeout: 8_000,
  });

  await popover.locator('[data-testid="relay-popover-close"]').click();
  await expect(popover).toBeHidden();
});

test("phone: popover renders inside the bottom sheet", async ({ browser }) => {
  const ctx = await browser.newContext({ viewport: { width: 390, height: 844 } });
  const page = await ctx.newPage();
  page.on("pageerror", (err) =>
    process.stderr.write(`[pageerror] ${err.stack || err}\n`),
  );
  await page.goto(buildUrl());

  // On phone the channels rail is inside the channels drawer; open it.
  // (The brand button at the top opens the rooms drawer; the channels
  // drawer is opened by a chevron / hamburger on the room header.)
  // If the relays section is reachable directly (e.g. when the channels
  // drawer is mounted but slid in), this becomes a no-op.
  const channelsDrawerOpener = page.locator(
    '[data-testid="phone-open-channels"]',
  );
  if (await channelsDrawerOpener.count()) {
    await channelsDrawerOpener.click();
  }

  await expect(page.locator('[data-testid="relays-section"]')).toBeVisible({
    timeout: 10_000,
  });
  const row = page.locator('[data-testid="relay-row"]').first();
  await expect(row).toHaveAttribute("data-relay-state", "connected", {
    timeout: 10_000,
  });
  await row.click();

  const popover = page.locator('[data-testid="relay-popover"]');
  await expect(popover).toBeVisible();
  // Assert the popover lives inside the bottom-sheet host. The host's
  // testid is `bottom-sheet`; a sibling `data-testid="bottom-sheet"`
  // ancestor proves this is the InSheet placement, not the Floating
  // desktop card mistakenly mounted.
  const sheetAncestor = popover.locator(
    'xpath=ancestor::*[@data-testid="bottom-sheet"]',
  );
  await expect(sheetAncestor).toHaveCount(1);

  await ctx.close();
});
```

If the phone-channels-drawer testid is named differently (search `rg -n 'phone-open\|ChannelsDrawer\|drawer.*open' web/src/`), substitute the real selector. If the channels rail is *always* reachable on phone without an explicit opener, the `if (await channelsDrawerOpener.count())` block becomes a harmless no-op.

- [ ] **Step 13.2: Run the new spec**

Run: `cd web && nix develop --command npx playwright test relays.spec.js 2>&1 | tail -40`
Expected: both tests PASS. If the desktop test fails on the RTT assertion within 8 s, double-check the heartbeat URL param made it through (Task 7 + Task 10) and that Client::new actually applies it (Task 6).

- [ ] **Step 13.3: Run the full e2e suite to catch regressions**

Run: `cd web && nix develop --command npx playwright test 2>&1 | tail -20`
Expected: no regressions. If `peer_status_popover.spec.js` or other tests fail because they relied on the now-removed Bridge fixture data, fix them.

- [ ] **Step 13.4: Commit**

```bash
git add web/e2e/relays.spec.js
git commit -m "$(cat <<'EOF'
web: e2e for Relays rail + popover (desktop + phone bottom sheet)

Spawns a real sunset-relay, asserts:
  * Relays section + row materialise with the right hostname.
  * Row state reaches "connected".
  * Popover opens with status pill, full addr, live RTT and
    heard-from age (the latter two driven by the new
    heartbeat_interval_ms=2000 URL param so the test fits under 15 s).
  * On phone viewport the popover lives inside the bottom-sheet host.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 14 — Cross-cutting verification

**Files:** none modified — verification only.

- [ ] **Step 14.1: Run the full Rust workspace test suite**

Run: `nix develop --command cargo test --workspace --all-features 2>&1 | tail -20`
Expected: all PASS.

- [ ] **Step 14.2: Run clippy with workspace policy (no suppressions allowed)**

Run: `nix develop --command cargo clippy --workspace --all-features --all-targets -- -D warnings 2>&1 | tail -30`
Expected: clean. If clippy flags anything new from this change, fix at source — `#[allow]` / `#[expect]` are forbidden.

Run: `nix develop --command bash scripts/check-no-clippy-allow.sh 2>&1 | tail`
Expected: empty (no suppressions added).

- [ ] **Step 14.3: Format check**

Run: `nix develop --command cargo fmt --all --check 2>&1 | tail`
Expected: clean. If it diffs, run `cargo fmt --all` and fold into Task 14's verification commit.

- [ ] **Step 14.4: Gleam build + tests**

Run: `cd web && nix develop --command gleam build 2>&1 | tail -10`
Run: `cd web && nix develop --command gleam test 2>&1 | tail -20`
Expected: both clean.

- [ ] **Step 14.5: Full Playwright suite (one more time, after all the Rust changes have been baked in)**

Run: `cd web && nix develop --command npx playwright test 2>&1 | tail -20`
Expected: all PASS.

- [ ] **Step 14.6: Mobile + desktop visual smoke**

CLAUDE.md requires browser verification for UI changes. Open the dev server, with a relay configured, exercise the golden path on both desktop and DevTools mobile (390×844): the Relays section is visible, click opens a popover with live metrics, close works, the desktop popover floats top-right while the phone version is the bottom sheet.

- [ ] **Step 14.7: No-op commit if everything passed; otherwise loop back**

If any check produced changes, commit them with `chore: post-implementation verification fixes`. Otherwise nothing to commit — the implementation is complete.

---

## Spec coverage cross-check

| Spec section | Tasks |
|---|---|
| §1 PongObserved + IntentSnapshot fields | T1, T2, T3, T4, T5 |
| §1 disconnect-preserves-liveness semantics | T5.4 + T5.1 (`disconnect_preserves_last_pong_and_rtt`) |
| §1 wire format unchanged | (no protocol change; only InboundEvent/EngineEvent) |
| §2 wasm bridge enrichment | T6 |
| §3 Domain types `Relay` + `RelayConnState`; remove Bridge/Minecraft | T8 |
| §3 View `rail_section` + `popover` (Floating/InSheet) | T11, T12 |
| §3 Wiring at Client construction | T7, T10 |
| §3 Channels-rail integration | T11 |
| §3 Mobile bottom sheet | T12, T13 (phone test) |
| §3 `data-testid` hooks | T11 (`relays-section`, `relay-row`, `data-relay-host/state`), T12 (`relay-popover*`) |
| §4 Fixture cleanup | T8 |
| Tests — Rust unit | T2.4, T3.2, T5.1 |
| Tests — Gleam pure | T9 |
| Tests — Playwright e2e | T13 |
| Out-of-scope items NOT implemented | (per spec — confirmed not in any task) |
