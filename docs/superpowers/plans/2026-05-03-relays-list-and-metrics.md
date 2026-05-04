# Relays list and metrics — implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the dummy "minecraft bridge" channels-rail entry with a real "Relays" list driven by the supervisor's existing `IntentSnapshot` stream, including live last-pong-at and round-trip-time per relay. Click-through opens a popover (floating on desktop, bottom sheet on phone) with full metrics.

**Architecture:** PR #23 already landed the `Connectable + IntentId` supervisor refactor and the `intents: Dict(Float, IntentSnapshot)` model in Gleam. This plan adds (a) liveness measurement: `EngineEvent::PongObserved` from the per-peer task → supervisor folds into new `IntentSnapshot.{last_pong_at_unix_ms, last_rtt_ms}` fields, (b) wasm-bridge enrichment of `IntentSnapshotJs` with the new fields and a `heartbeat_interval_ms` constructor arg, and (c) the Gleam `Relay` view-model + `views/relays.gleam` (rail section + popover) on top of the already-live `IntentChanged` stream. Direct WebRTC peers are filtered out by inspecting the `label` field's scheme (`webrtc://...`).

**Tech Stack:** Rust workspace (stable), tokio (current_thread), `web_time::SystemTime` for cross-platform wall-clock, wasm-bindgen, Gleam (lustre, gleam/uri), Playwright.

**Spec:** [`docs/superpowers/specs/2026-05-03-relays-list-and-metrics-design.md`](../specs/2026-05-03-relays-list-and-metrics-design.md)

---

## File structure

**Modify (Rust — sunset-sync):**
- `crates/sunset-sync/src/peer.rs` — change the `pong_tx`/`pong_rx` channel to carry the Pong's nonce; in the liveness loop, stamp `last_ping_sent_at` on each send and on Pong receipt compute `rtt_ms` and emit `InboundEvent::PongObserved`.
- `crates/sunset-sync/src/engine.rs` — add `InboundEvent::PongObserved` and `EngineEvent::PongObserved` variants; `handle_inbound_event` re-emits.
- `crates/sunset-sync/src/supervisor.rs` — add `last_pong_at_unix_ms` / `last_rtt_ms` to `IntentEntry` + `IntentSnapshot`; add a `PongObserved` arm to `handle_engine_event`; update `broadcast` and the `Snapshot` builder to emit the new fields; preserve them across `Backoff` transitions.

**Modify (Rust — sunset-web-wasm):**
- `crates/sunset-web-wasm/src/intent.rs` — add `last_pong_at_unix_ms: Option<f64>` and `last_rtt_ms: Option<f64>` to `IntentSnapshotJs`; update the `From<&IntentSnapshot>` impl.
- `crates/sunset-web-wasm/src/client.rs` — `Client::new` gains a `heartbeat_interval_ms: u32` (0 = use SyncConfig default of 15 s); when non-zero, sets `SyncConfig.heartbeat_interval = Duration::from_millis(n)` and `heartbeat_timeout = 3 × interval`.

**Modify (Gleam):**
- `web/src/sunset_web/sunset.ffi.mjs` — `createClient` shim grows `heartbeatIntervalMs` arg and forwards to `new Client(seed, hb)`; `onIntentChanged` shim's `new IntentSnapshot(...)` constructor call grows two args reading `snap.last_pong_at_unix_ms` / `snap.last_rtt_ms`; new `heartbeatIntervalMsFromUrl()` shim.
- `web/src/sunset_web/sunset.gleam` — `IntentSnapshot` record gains two `Option(Int)` fields; `create_client` external grows the heartbeat arg; new `heartbeat_interval_ms_from_url` external.
- `web/src/sunset_web/domain.gleam` — drop `Bridge(BridgeKind)` from `ChannelKind`; drop `BridgeKind`, `BridgeOpt`, `HasBridge`, `NoBridge`, `BridgeRelay`; drop `bridge:` from `Room` / `Member` / `Message`; add `RelayConnState` and `Relay` view-model types.
- `web/src/sunset_web/fixture.gleam` — drop the `minecraft-bridge` channel and every `bridge:` field; update imports.
- `web/src/sunset_web/views/main_panel.gleam` — drop `bridge_tag` and the `case m.bridge` branch.
- `web/src/sunset_web/views/channels.gleam` — replace the `bridge_channels` filter and "Bridges" section with `relays_view.rail_section(...)`; add `relays` and `on_open_relay` parameters.
- `web/src/sunset_web.gleam` — Model gains `relays_popover: Option(Float)`; new Msgs `OpenRelayPopover(Float)` / `CloseRelayPopover`; pass heartbeat URL param to `create_client`; add `relays_for_view(model.intents)` view-helper; mount popover overlays alongside `peer_status_popover_overlay` (Floating on desktop, in-bottom_sheet on phone).
- `web/test/relay_status_pill_test.gleam` — fix `IntentSnapshot` constructor calls to pass `option.None, option.None` for the two new fields. No assertion changes.

**Create (Rust unit tests):** appended into `peer.rs`, `engine.rs`, `supervisor.rs` `#[cfg(test)]` blocks.

**Create (Gleam):**
- `web/src/sunset_web/views/relays.gleam` — `rail_section`, `popover` (with `Placement = Floating | InSheet`), pure helpers (`is_relay_label`, `parse_host`, `parse_state`, `format_status`, `format_rtt`, `humanize_age`, `short_peer_id`, `from_intent`, `relays_for_view`).
- `web/test/sunset_web/views/relays_test.gleam` — pure-function tests.
- `web/e2e/relays.spec.js` — Playwright covering desktop + phone bottom sheet.

---

## Task 1 — Add `EngineEvent::PongObserved` + `InboundEvent::PongObserved`

**Files:**
- Modify: `crates/sunset-sync/src/engine.rs:90-98` (EngineEvent variants)
- Modify: `crates/sunset-sync/src/peer.rs:15-52` (InboundEvent variants)

- [ ] **Step 1.1: Add `InboundEvent::PongObserved` variant**

In `crates/sunset-sync/src/peer.rs`, append a variant to `InboundEvent`:

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

- [ ] **Step 1.3: Verify the workspace still builds**

Run: `nix develop --command cargo build -p sunset-sync 2>&1 | tail -10`
Expected: builds. If a `match event` over `InboundEvent` errors with "non-exhaustive patterns: `PongObserved { .. }` not covered", add a temporary `InboundEvent::PongObserved { .. } => {}` arm in `handle_inbound_event` so the build is green; Task 3 replaces it with the real handler.

- [ ] **Step 1.4: Commit**

```bash
git add crates/sunset-sync/src/engine.rs crates/sunset-sync/src/peer.rs
git commit -m "$(cat <<'EOF'
sunset-sync: add InboundEvent and EngineEvent PongObserved variants

Carries per-peer liveness (RTT and wall-clock observed-at) so the
supervisor can surface it to applications via IntentSnapshot. No
producer or consumer wired yet — populated in subsequent tasks.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 2 — Liveness loop measures RTT and emits `InboundEvent::PongObserved`

**Files:**
- Modify: `crates/sunset-sync/src/peer.rs` (`recv_reliable_task` Pong arm, `liveness_task`)

- [ ] **Step 2.1: Change `pong_tx`/`pong_rx` to carry the Pong's nonce**

In `crates/sunset-sync/src/peer.rs:186`, replace the `pong_tx` channel definition:

```rust
    // Pong delivery channel: recv_reliable_task forwards every observed
    // Pong's nonce here so the liveness_task can update last_pong_at AND
    // emit PongObserved with measured RTT, without sharing mutable state
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

Replace the entire `liveness_task` block in `crates/sunset-sync/src/peer.rs` (currently lines 305-359) with:

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
Expected: builds. If the `handle_inbound_event` placeholder from Step 1.3 isn't yet in place, add it now (`InboundEvent::PongObserved { .. } => {}`).

- [ ] **Step 2.4: Add a peer-task unit test**

Inspect `crates/sunset-sync/src/test_transport.rs` to learn the actual fixture API (struct names, `feed`/`drain_one`-style methods). Append to the existing `#[cfg(test)] mod tests` block in `crates/sunset-sync/src/peer.rs` (or create one). The test drives the per-peer task, responds Hello + Pong, and asserts an `InboundEvent::PongObserved` lands on `inbound_rx`:

```rust
#[cfg(test)]
mod liveness_pong_observed_tests {
    use super::*;
    use crate::engine::ConnectionId;
    use crate::transport::TransportKind;
    use std::time::Duration;
    use sunset_store::VerifyingKey;

    fn peer(b: &[u8; 32]) -> PeerId {
        PeerId(VerifyingKey::new(Bytes::copy_from_slice(b)))
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn liveness_emits_pong_observed_with_rtt() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                // Adapt to the actual TestTransport / InMemoryPair API
                // exposed by `crate::test_transport`. Concretely we need:
                //   * a connection that the per-peer task drives, and
                //   * a "remote" handle to inject Hello + Pong and to
                //     drain whatever the local side sends.
                let pair = crate::test_transport::InMemoryPair::new(
                    TransportKind::Secondary,
                );
                let local_id = peer(&[1u8; 32]);
                let remote_id = peer(&[2u8; 32]);
                let env = PeerEnv {
                    local_peer: local_id.clone(),
                    protocol_version: crate::types::SyncConfig::default()
                        .protocol_version,
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
                pair.remote
                    .feed(SyncMessage::Hello {
                        protocol_version: crate::types::SyncConfig::default()
                            .protocol_version,
                        peer_id: remote_id.clone(),
                    })
                    .await;
                let _hello_out = pair.remote.drain_one().await.expect("hello out");
                tokio::time::advance(Duration::from_millis(25)).await;
                let ping = pair.remote.drain_one().await.expect("ping out");
                let nonce = match ping {
                    SyncMessage::Ping { nonce } => nonce,
                    other => panic!("expected Ping, got {other:?}"),
                };
                pair.remote.feed(SyncMessage::Pong { nonce }).await;
                let mut found = None;
                for _ in 0..32 {
                    if let Ok(Some(ev)) = tokio::time::timeout(
                        Duration::from_millis(50),
                        inbound_rx.recv(),
                    )
                    .await
                    {
                        if let InboundEvent::PongObserved { peer_id, rtt_ms, .. } = &ev {
                            assert_eq!(peer_id, &remote_id);
                            found = Some(*rtt_ms);
                            break;
                        }
                    }
                }
                let rtt = found.expect("no PongObserved seen");
                assert!(rtt < 5_000, "RTT should be small under paused time");
            })
            .await;
    }
}
```

If the `InMemoryPair` / `feed` / `drain_one` shape differs in `test_transport.rs`, adapt to whatever exists. The test's contract — drive Hello → expect Ping → respond Pong → assert `InboundEvent::PongObserved` — does not change. If no per-peer-task test infra exists at all, write a minimal `Conn` mock inline that satisfies `TransportConnection`; do not give up on the test.

- [ ] **Step 2.5: Run the test**

Run: `nix develop --command cargo test -p sunset-sync liveness_pong_observed -- --nocapture 2>&1 | tail -30`
Expected: PASS.

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
- Modify: `crates/sunset-sync/src/engine.rs` (`handle_inbound_event`)

- [ ] **Step 3.1: Replace the placeholder arm with the real handler**

In `crates/sunset-sync/src/engine.rs`, find the placeholder added in Step 1.3 (or 2.3) inside `handle_inbound_event`, and replace it with:

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

Append to the existing tests module in `crates/sunset-sync/src/engine.rs` (locate any current `#[tokio::test]` to mirror its setup helpers):

```rust
#[tokio::test(flavor = "current_thread")]
async fn pong_observed_inbound_event_propagates_as_engine_event() {
    use crate::peer::InboundEvent;
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            // Build a minimal SyncEngine using the same constructor the
            // existing tests use. (Mirror the helper that test files like
            // crates/sunset-sync/tests/two_peer_sync.rs reach for.)
            let (engine, _store) = crate::engine::tests::make_engine().await;
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

If `tests::make_engine` doesn't exist, locate an analogous helper in the engine tests module (or in `tests/`) and adapt the call. If there is genuinely no helper, build a minimal engine inline using `MemoryStore` + `NopTransport` (as `peer/mod.rs::tests::helpers` does).

- [ ] **Step 3.3: Run**

Run: `nix develop --command cargo test -p sunset-sync pong_observed_inbound_event_propagates -- --nocapture 2>&1 | tail -20`
Expected: PASS.

- [ ] **Step 3.4: Commit**

```bash
git add crates/sunset-sync/src/engine.rs
git commit -m "$(cat <<'EOF'
sunset-sync: engine re-emits PongObserved as EngineEvent

Per-peer task → InboundEvent::PongObserved → handle_inbound_event
forwards to all engine-event subscribers (PeerSupervisor consumes it
in the next change).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 4 — Supervisor `IntentEntry` and `IntentSnapshot` gain liveness fields

**Files:**
- Modify: `crates/sunset-sync/src/supervisor.rs`

- [ ] **Step 4.1: Add fields to `IntentEntry` and `IntentSnapshot`**

In `crates/sunset-sync/src/supervisor.rs`, add the two fields. Add to `IntentSnapshot` (currently around lines 72-82) — after `pub label: String,`:

```rust
    /// Wall-clock ms of the most recent Pong observed from this peer.
    /// `None` until the first Pong of the *first* connection lands.
    /// Preserved across Backoff transitions (the popover should show
    /// "heard from 12s ago" while reconnecting), cleared only when
    /// the intent itself is removed (`SupervisorCommand::Remove`).
    pub last_pong_at_unix_ms: Option<u64>,
    /// Round-trip time of the most recent Pong, in milliseconds.
    /// `None` under the same conditions as `last_pong_at_unix_ms`.
    pub last_rtt_ms: Option<u64>,
```

Add the same two fields to `IntentEntry` (around lines 93-107) — after `pub label: String,`.

- [ ] **Step 4.2: Update every `IntentEntry { ... }` literal**

Find every site that builds an `IntentEntry`:

```bash
nix develop --command rg -n 'IntentEntry \{' crates/sunset-sync/src/supervisor.rs
```

Add `last_pong_at_unix_ms: None, last_rtt_ms: None,` to each.

- [ ] **Step 4.3: Update `broadcast` to emit the new fields**

Find the `fn broadcast(...)` (currently around lines 186-197) and update the `IntentSnapshot { ... }` literal it constructs to include:

```rust
            last_pong_at_unix_ms: entry.last_pong_at_unix_ms,
            last_rtt_ms: entry.last_rtt_ms,
```

- [ ] **Step 4.4: Update the `Snapshot` command builder**

Find `SupervisorCommand::Snapshot` (currently around line 407) and the `.map(|(id, e)| IntentSnapshot { ... })` closure. Add the same two fields.

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

- [ ] **Step 5.1: Inspect the current supervisor test setup**

Read `crates/sunset-sync/src/supervisor.rs` lines around 580-790 to find the test fixture used by `subscribe_emits_state_transitions`, `subscribe_observes_connecting_during_redial`, etc. Identify the helper for two-peer setup and the way EngineEvents are injected. (We need to mirror its idioms; the new tests below shouldn't reinvent that wheel.)

- [ ] **Step 5.2: Write three failing supervisor unit tests**

Append to the `#[cfg(test)] mod tests` block in `crates/sunset-sync/src/supervisor.rs`. The exact helper names below (`make_two_peer_setup`, `inject_engine_event`) are placeholders — substitute the real ones discovered in 5.1:

```rust
#[tokio::test(flavor = "current_thread")]
async fn pong_observed_updates_intent_snapshot() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            // Two-peer setup from the existing test fixtures.
            let (alice, bob, bob_addr) = make_two_peer_setup().await;
            let sup = PeerSupervisor::new(alice.clone(), BackoffPolicy::default());
            crate::spawn::spawn_local({
                let s = sup.clone();
                async move { s.run().await }
            });
            let id = sup
                .add(crate::connectable::Connectable::Direct(bob_addr.clone()))
                .await
                .expect("add bob");
            // Wait for Connected. Subscribers replay current state, so
            // we can poll snapshot() rather than draining the channel.
            for _ in 0..200 {
                if sup
                    .snapshot()
                    .await
                    .iter()
                    .any(|s| s.id == id && s.state == IntentState::Connected)
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
            let bob_pid = bob.local_peer_id();
            inject_engine_event(
                &alice,
                EngineEvent::PongObserved {
                    peer_id: bob_pid.clone(),
                    rtt_ms: 17,
                    observed_at_unix_ms: 1_700_000_000_500,
                },
            )
            .await;
            // Snapshot reflects the new fields.
            for _ in 0..200 {
                let snap = sup
                    .snapshot()
                    .await
                    .into_iter()
                    .find(|s| s.id == id)
                    .expect("intent");
                if snap.last_rtt_ms == Some(17)
                    && snap.last_pong_at_unix_ms == Some(1_700_000_000_500)
                {
                    return;
                }
                tokio::task::yield_now().await;
            }
            panic!("PongObserved did not propagate to IntentSnapshot");
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn pong_observed_for_unknown_peer_is_dropped() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (alice, _bob, _bob_addr) = make_two_peer_setup().await;
            let sup = PeerSupervisor::new(alice.clone(), BackoffPolicy::default());
            crate::spawn::spawn_local({
                let s = sup.clone();
                async move { s.run().await }
            });
            let mut sub = sup.subscribe_intents().await;
            // Drain any startup snapshots (none here — no intents yet).
            let _ = tokio::time::timeout(
                std::time::Duration::from_millis(50),
                sub.recv(),
            )
            .await;
            let stranger = PeerId(sunset_store::VerifyingKey::new(
                bytes::Bytes::from_static(&[99u8; 32]),
            ));
            inject_engine_event(
                &alice,
                EngineEvent::PongObserved {
                    peer_id: stranger,
                    rtt_ms: 1,
                    observed_at_unix_ms: 1,
                },
            )
            .await;
            let r = tokio::time::timeout(std::time::Duration::from_millis(100), sub.recv()).await;
            assert!(r.is_err(), "expected no broadcast for unknown peer");
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn disconnect_preserves_last_pong_and_rtt() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (alice, bob, bob_addr) = make_two_peer_setup().await;
            let sup = PeerSupervisor::new(alice.clone(), BackoffPolicy::default());
            crate::spawn::spawn_local({
                let s = sup.clone();
                async move { s.run().await }
            });
            let id = sup
                .add(crate::connectable::Connectable::Direct(bob_addr.clone()))
                .await
                .expect("add");
            // Wait for Connected.
            for _ in 0..200 {
                if sup
                    .snapshot()
                    .await
                    .iter()
                    .any(|s| s.id == id && s.state == IntentState::Connected)
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
            let bob_pid = bob.local_peer_id();
            inject_engine_event(
                &alice,
                EngineEvent::PongObserved {
                    peer_id: bob_pid.clone(),
                    rtt_ms: 11,
                    observed_at_unix_ms: 5_000,
                },
            )
            .await;
            // Wait for fold.
            for _ in 0..200 {
                let snap = sup
                    .snapshot()
                    .await
                    .into_iter()
                    .find(|s| s.id == id)
                    .expect("intent");
                if snap.last_rtt_ms == Some(11) {
                    break;
                }
                tokio::task::yield_now().await;
            }
            // Force a disconnect.
            alice.remove_peer(bob_pid.clone()).await.ok();
            for _ in 0..200 {
                let snap = sup
                    .snapshot()
                    .await
                    .into_iter()
                    .find(|s| s.id == id)
                    .expect("intent");
                if snap.state == IntentState::Backoff {
                    assert_eq!(snap.last_rtt_ms, Some(11));
                    assert_eq!(snap.last_pong_at_unix_ms, Some(5_000));
                    return;
                }
                tokio::task::yield_now().await;
            }
            panic!("intent never transitioned to Backoff");
        })
        .await;
}
```

If `make_two_peer_setup` and `inject_engine_event` don't exist verbatim, adapt to the real helpers found in Step 5.1. If `inject_engine_event` requires a public test-only entry point (e.g., the engine has only a private `emit_engine_event`), add a `#[cfg(test)] pub(crate) async fn emit_engine_event_for_test(...)` shim on `SyncEngine` that wraps the private call. Do not call private methods from tests via reflection — add the test-only public shim.

- [ ] **Step 5.3: Run — expect failures**

Run: `nix develop --command cargo test -p sunset-sync supervisor::tests::pong_observed -- --nocapture 2>&1 | tail -30`
Run: `nix develop --command cargo test -p sunset-sync supervisor::tests::disconnect_preserves -- --nocapture 2>&1 | tail -30`
Expected: `pong_observed_updates_intent_snapshot` and `disconnect_preserves_last_pong_and_rtt` FAIL (last_rtt_ms is None). `pong_observed_for_unknown_peer_is_dropped` may pass trivially today since there's no handler at all.

- [ ] **Step 5.4: Add the PongObserved arm in `handle_engine_event`**

In `crates/sunset-sync/src/supervisor.rs`, locate `handle_engine_event` and add a third arm:

```rust
            EngineEvent::PongObserved { peer_id, rtt_ms, observed_at_unix_ms } => {
                let mut state = self.state.borrow_mut();
                let id = match state.peer_to_intent.get(&peer_id) {
                    Some(i) => *i,
                    None => return,
                };
                if let Some(entry) = state.intents.get_mut(&id) {
                    entry.last_pong_at_unix_ms = Some(observed_at_unix_ms);
                    entry.last_rtt_ms = Some(rtt_ms);
                }
                Self::broadcast(&mut state, id);
            }
```

If `Self::broadcast` takes `&PeerAddr` instead of `IntentId`, adapt — read the existing `broadcast` signature and call it with whatever key it expects.

- [ ] **Step 5.5: Verify `PeerRemoved` does NOT clear the liveness fields**

Re-read the `EngineEvent::PeerRemoved` arm. It must mutate `state` and `peer_id` only; it must NOT touch `last_pong_at_unix_ms` or `last_rtt_ms`. If it does, fix it.

- [ ] **Step 5.6: Run the tests — expect PASS**

Run: `nix develop --command cargo test -p sunset-sync supervisor:: -- --nocapture 2>&1 | tail -40`
Expected: all three new tests PASS, no regressions in existing supervisor tests.

- [ ] **Step 5.7: Run the full sunset-sync test suite**

Run: `nix develop --command cargo test -p sunset-sync 2>&1 | tail -10`
Expected: clean.

- [ ] **Step 5.8: Commit**

```bash
git add crates/sunset-sync/src/supervisor.rs crates/sunset-sync/src/engine.rs
git commit -m "$(cat <<'EOF'
sunset-sync: supervisor folds PongObserved into IntentSnapshot

handle_engine_event grows a PongObserved arm that updates the
IntentEntry's last_pong_at_unix_ms / last_rtt_ms and broadcasts the
new snapshot. Stale events (peer not in peer_to_intent) are dropped
silently. Disconnect preserves the fields; only Remove clears them
(by removing the entry entirely).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 6 — wasm bridge: `IntentSnapshotJs` liveness, `Client::new` heartbeat override

**Files:**
- Modify: `crates/sunset-web-wasm/src/intent.rs`
- Modify: `crates/sunset-web-wasm/src/client.rs`

- [ ] **Step 6.1: Add fields and mapping in `intent.rs`**

In `crates/sunset-web-wasm/src/intent.rs`, append two fields to `IntentSnapshotJs`:

```rust
    pub last_pong_at_unix_ms: Option<f64>,
    pub last_rtt_ms: Option<f64>,
```

Update the `From<&IntentSnapshot> for IntentSnapshotJs` impl to add:

```rust
            last_pong_at_unix_ms: s.last_pong_at_unix_ms.map(|n| n as f64),
            last_rtt_ms: s.last_rtt_ms.map(|n| n as f64),
```

(`u64 → f64` is safe for ms values in any plausible range.)

- [ ] **Step 6.2: Update `Client::new` to accept `heartbeat_interval_ms`**

In `crates/sunset-web-wasm/src/client.rs:57`, change the constructor signature:

```rust
    #[wasm_bindgen(constructor)]
    pub fn new(seed: &[u8], heartbeat_interval_ms: u32) -> Result<Client, JsError> {
```

Inside, after the existing `let multi = ...;` line and before the `SyncEngine::new(...)` call (currently around line 78), build the config:

```rust
        let mut config = SyncConfig::default();
        if heartbeat_interval_ms > 0 {
            let interval = std::time::Duration::from_millis(heartbeat_interval_ms as u64);
            config.heartbeat_interval = interval;
            // Match default 3× ratio between interval and timeout.
            config.heartbeat_timeout = interval * 3;
        }
```

Replace the `SyncConfig::default()` argument in the `SyncEngine::new(...)` call with `config`.

- [ ] **Step 6.3: Build the wasm crate**

Run: `nix develop --command cargo build -p sunset-web-wasm --target wasm32-unknown-unknown 2>&1 | tail -15`
Expected: builds.

- [ ] **Step 6.4: Build the rest of the workspace (catches downstream callers of `Client::new`)**

Run: `nix develop --command cargo build --workspace --all-features 2>&1 | tail -15`
Expected: any tests that build a `Client` directly (e.g., `crates/sunset-web-wasm/tests/construct.rs`) need a `0` arg added. Find them with:

```bash
nix develop --command rg -n 'Client::new\b|new Client\(' crates/ web/
```

Update each.

- [ ] **Step 6.5: Commit**

```bash
git add crates/sunset-web-wasm/src/intent.rs crates/sunset-web-wasm/src/client.rs crates/sunset-web-wasm/tests/
git commit -m "$(cat <<'EOF'
sunset-web-wasm: optional heartbeat override + liveness on IntentSnapshotJs

Client::new gains a u32 heartbeat_interval_ms (0 = use the SyncConfig
default of 15 s), mirroring presence_interval-style URL-tunability for
e2e tests. IntentSnapshotJs gains last_pong_at_unix_ms / last_rtt_ms
optional fields; the From<&IntentSnapshot> impl picks them up
automatically.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 7 — Gleam FFI: heartbeat URL param + `IntentSnapshot` extension

**Files:**
- Modify: `web/src/sunset_web/sunset.ffi.mjs`
- Modify: `web/src/sunset_web/sunset.gleam`
- Modify: `web/test/relay_status_pill_test.gleam`

- [ ] **Step 7.1: Update `createClient` shim to pass heartbeat**

In `web/src/sunset_web/sunset.ffi.mjs`, find the existing `createClient` export and replace with:

```javascript
export async function createClient(seed, heartbeatIntervalMs, callback) {
  await ensureLoaded();
  const seedBytes = bitsToBytes(seed);
  const hb =
    Number.isFinite(heartbeatIntervalMs) && heartbeatIntervalMs > 0
      ? heartbeatIntervalMs
      : 0;
  const client = new Client(seedBytes, hb);
  if (typeof window !== "undefined" && window.SUNSET_TEST) {
    window.sunsetClient = client;
  }
  callback(client);
}
```

- [ ] **Step 7.2: Update `onIntentChanged` to wrap the new fields**

In the `onIntentChanged` shim (currently around lines 108-128), update the `new IntentSnapshot(...)` call to include two more args:

```javascript
    const lastPongMs = snap.last_pong_at_unix_ms;
    const lastRttMs = snap.last_rtt_ms;
    const record = new IntentSnapshot(
      snap.id,
      snap.state,
      snap.label,
      peerPubkey === undefined || peerPubkey === null
        ? new None()
        : new Some(new BitArray(peerPubkey)),
      kind === undefined || kind === null ? new None() : new Some(kind),
      snap.attempt,
      lastPongMs === undefined || lastPongMs === null
        ? new None()
        : new Some(lastPongMs),
      lastRttMs === undefined || lastRttMs === null
        ? new None()
        : new Some(lastRttMs),
    );
    callback(record);
```

- [ ] **Step 7.3: Add `heartbeatIntervalMsFromUrl` shim**

Append to `web/src/sunset_web/sunset.ffi.mjs`:

```javascript
/// Read `?heartbeat_interval_ms=NNN` from the current URL. Returns 0
/// when absent or unparseable, signalling Client::new to use the
/// SyncConfig default (15 s). e2e-only knob.
export function heartbeatIntervalMsFromUrl() {
  const params = new URLSearchParams(window.location.search);
  const raw = params.get("heartbeat_interval_ms");
  if (raw === null) return 0;
  const n = Number(raw);
  return Number.isFinite(n) && n > 0 ? n : 0;
}
```

- [ ] **Step 7.4: Update Gleam externals + `IntentSnapshot` record**

In `web/src/sunset_web/sunset.gleam`:

Change `create_client`:

```gleam
@external(javascript, "./sunset.ffi.mjs", "createClient")
pub fn create_client(
  seed: BitArray,
  heartbeat_interval_ms: Int,
  callback: fn(ClientHandle) -> Nil,
) -> Nil
```

Add the URL helper near the bottom (alongside `presence_params_from_url`):

```gleam
/// Read `?heartbeat_interval_ms=NNN` from the URL. Returns 0 when
/// absent or unparseable. e2e-only knob.
@external(javascript, "./sunset.ffi.mjs", "heartbeatIntervalMsFromUrl")
pub fn heartbeat_interval_ms_from_url() -> Int
```

Extend `IntentSnapshot`:

```gleam
pub type IntentSnapshot {
  IntentSnapshot(
    id: Float,
    state: String,
    label: String,
    peer_pubkey: option.Option(BitArray),
    kind: option.Option(String),
    attempt: Int,
    /// Wall-clock ms of the most recent Pong. None until the first
    /// Pong of the first connection lands; preserved across Backoff.
    last_pong_at_ms: option.Option(Int),
    /// Round-trip time of the most recent Pong, in milliseconds.
    last_rtt_ms: option.Option(Int),
  )
}
```

- [ ] **Step 7.5: Fix `relay_status_pill_test.gleam`**

In `web/test/relay_status_pill_test.gleam`, update the `snap` helper:

```gleam
fn snap(id: Float, state: String) -> IntentSnapshot {
  IntentSnapshot(
    id: id,
    state: state,
    label: "test",
    peer_pubkey: None,
    kind: None,
    attempt: 0,
    last_pong_at_ms: None,
    last_rtt_ms: None,
  )
}
```

- [ ] **Step 7.6: Compile-check**

Run: `cd web && nix develop --command gleam build 2>&1 | tail -15`
Expected: build fails because `sunset_web.gleam` still calls `sunset.create_client(seed, callback)` without the new arg. We fix that in Task 9.

Run: `cd web && nix develop --command gleam test 2>&1 | tail -20`
Expected: `relay_status_pill_test` passes (the constructor extension is the only relevant change). Other test failures may exist due to pending Bridge cleanup; those land in Task 8.

- [ ] **Step 7.7: Commit**

```bash
git add web/src/sunset_web/sunset.ffi.mjs web/src/sunset_web/sunset.gleam web/test/relay_status_pill_test.gleam
git commit -m "$(cat <<'EOF'
web: extend IntentSnapshot with last_pong_at_ms / last_rtt_ms

The FFI shim wraps the new IntentSnapshotJs fields; the Gleam record
gains two Option(Int) fields. createClient grows a heartbeat-interval
arg sourced from a new ?heartbeat_interval_ms URL param (e2e-only;
default 0 means use the SyncConfig 15 s default). relay_status_pill_test
constructor calls updated mechanically.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 8 — Domain types + remove Bridge / Minecraft fixture types

**Files:**
- Modify: `web/src/sunset_web/domain.gleam`
- Modify: `web/src/sunset_web/fixture.gleam`
- Modify: `web/src/sunset_web/views/main_panel.gleam`
- Modify: `web/src/sunset_web/views/channels.gleam`
- Modify: `web/test/fixture_test.gleam` (if it constructs domain types directly)

- [ ] **Step 8.1: Drop Bridge from domain.gleam, add Relay types**

In `web/src/sunset_web/domain.gleam`, delete `BridgeKind`, `BridgeOpt`, change `ChannelKind` to:

```gleam
pub type ChannelKind {
  TextChannel
  Voice
}
```

Remove `bridge: BridgeOpt,` from `Room`, `Member`, `Message`. Drop `BridgeRelay` from the `RelayStatus` enum.

Append:

```gleam
pub type RelayConnState {
  RelayConnecting
  RelayConnected
  RelayBackoff
  RelayCancelled
}

/// View-model for a relay row + popover. Derived per render from
/// `Model.intents` via `relays_view.relays_for_view`. Not a source
/// of truth — `intents` remains so.
pub type Relay {
  Relay(
    /// IntentId — popover key.
    id: Float,
    /// Parsed hostname for display, e.g. "relay.sunset.chat".
    host: String,
    /// Full Connectable label (raw user input or canonical URL).
    raw_label: String,
    state: RelayConnState,
    attempt: Int,
    /// First 4 + last 4 hex bytes of the relay's peer_id. None
    /// while the Noise handshake is still pending.
    peer_id_short: option.Option(String),
    /// Wall-clock ms of the most recent Pong from this relay.
    last_pong_at_ms: option.Option(Int),
    /// Round-trip time of the most recent Pong, in milliseconds.
    last_rtt_ms: option.Option(Int),
  )
}
```

- [ ] **Step 8.2: Strip Bridge from fixture.gleam**

In `web/src/sunset_web/fixture.gleam`:
- Update the `import sunset_web/domain.{...}` line to drop `Bridge`, `BridgeRelay`, `HasBridge`, `Minecraft`, `NoBridge`.
- Remove the channel whose name is `"minecraft-bridge"` (currently around line 130-134).
- Remove every `bridge: NoBridge,`, `bridge: HasBridge(Minecraft),` field from constructors.
- For `Receipt(name: ..., relay: BridgeRelay)` rows, change `BridgeRelay` to `NoRelay` (only fixture data; nothing real).

- [ ] **Step 8.3: Strip Bridge from main_panel.gleam**

In `web/src/sunset_web/views/main_panel.gleam`:
- Remove `HasBridge`, `Minecraft`, `NoBridge` from the `domain.{...}` import.
- Delete the `case m.bridge { HasBridge(Minecraft) -> [bridge_tag(...)] NoBridge -> [] }` block (currently around lines 647-649); replace with `[]`.
- Delete `fn bridge_tag(...)` (currently around line 1025).

- [ ] **Step 8.4: Strip Bridge from channels.gleam**

In `web/src/sunset_web/views/channels.gleam`:
- Remove `Bridge`, `Minecraft` from the `domain.{...}` import.
- Delete the `bridge_channels` filter (currently lines 34-40) and the `case bridge_channels { ... } -> section(p, "Bridges", ...)` block (currently lines 94-102). Leave `element.fragment([])` where the section was — Task 10 replaces it with the real Relays section.
- Delete `fn bridge_channel_row(...)` (currently around line 871).

- [ ] **Step 8.5: Build and fix any remaining fallout**

Run: `cd web && nix develop --command gleam build 2>&1 | tail -25`
Expected: errors only around `create_client` arity (Task 9 fixes). Read each error and fix any missed `bridge:` occurrence or stale import.

Run: `cd web && nix develop --command gleam test 2>&1 | tail -20`
Expected: any test that constructed `Channel` / `Member` / `Message` / `Room` with `bridge:` needs that field dropped. The most likely culprit is `web/test/fixture_test.gleam`. Update.

- [ ] **Step 8.6: Commit**

```bash
git add web/src/sunset_web/domain.gleam web/src/sunset_web/fixture.gleam web/src/sunset_web/views/main_panel.gleam web/src/sunset_web/views/channels.gleam web/test/
git commit -m "$(cat <<'EOF'
web: remove Bridge / Minecraft fixture types; add Relay view-model

The dummy minecraft-bridge channel and the bridge: fields on Room /
Member / Message had no real producers; nothing in the runtime read
them. Replaced by a domain.Relay view-model derived per render from
Model.intents (added in the next change).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 9 — `relays.gleam` pure helpers + Gleam unit tests

**Files:**
- Create: `web/src/sunset_web/views/relays.gleam` (helpers only — view comes in Task 10)
- Create: `web/test/sunset_web/views/relays_test.gleam`

- [ ] **Step 9.1a: Add a `bitsToHex` FFI shim**

Append to `web/src/sunset_web/sunset.ffi.mjs`:

```javascript
/// Encode a BitArray (Uint8Array internally) as lowercase hex.
/// Used by the relays popover to render the relay's peer_id.
export function bitsToHex(bits) {
  const bytes = bitsToBytes(bits);
  return Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");
}
```

Add the Gleam external in `web/src/sunset_web/sunset.gleam` (near `client_public_key_hex`):

```gleam
@external(javascript, "./sunset.ffi.mjs", "bitsToHex")
pub fn bits_to_hex(bits: BitArray) -> String
```

- [ ] **Step 9.1b: Create `relays.gleam` with the pure helpers**

```gleam
//// Relays UI: rail-section list + click-through popover (desktop
//// floating / phone bottom sheet). This file currently exposes the
//// pure helpers and a from_intent / relays_for_view derivation. The
//// view functions land in the next change.

import gleam/dict.{type Dict}
import gleam/int
import gleam/list
import gleam/option.{type Option}
import gleam/order
import gleam/string
import gleam/uri
import sunset_web/domain.{
  type Relay, type RelayConnState, Relay, RelayBackoff, RelayCancelled,
  RelayConnected, RelayConnecting,
}
import sunset_web/sunset.{type IntentSnapshot}

/// True when `label` is a relay (not a direct WebRTC peer).
/// Connectable::Direct(webrtc://…) carries that scheme on its label;
/// every other shape (Resolving inputs like "relay.sunset.chat" or
/// Direct(wss://…) URLs from ?relay=) is a relay.
pub fn is_relay_label(label: String) -> Bool {
  !string.starts_with(label, "webrtc://")
}

/// Best-effort hostname extraction. When `label` looks like a URL
/// (contains `://`), use gleam/uri to extract host[:port]. When it's
/// a bare hostname (typical for Resolving inputs), return it
/// unchanged. Returns `label` on parse failure — defensive fallback so
/// a malformed label never crashes the rail.
pub fn parse_host(label: String) -> String {
  case string.contains(label, "://") {
    False -> label
    True ->
      case uri.parse(label) {
        Ok(u) ->
          case u.host {
            option.Some(h) ->
              case u.port {
                option.Some(p) -> h <> ":" <> int.to_string(p)
                option.None -> h
              }
            option.None -> label
          }
        Error(_) -> label
      }
  }
}

/// Map JS-side intent state string to the typed enum. Unknown
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

/// Format a hex pubkey as "first8…last8" (8 chars on each side).
/// Strings of length ≤ 16 are returned unchanged.
pub fn short_peer_id(hex: String) -> String {
  case string.length(hex) {
    n if n <= 16 -> hex
    n -> string.slice(hex, 0, 8) <> "…" <> string.slice(hex, n - 8, 8)
  }
}

/// Build a domain.Relay from a sunset.IntentSnapshot. Pure projection.
pub fn from_intent(snap: IntentSnapshot) -> Relay {
  let peer_id_short =
    snap.peer_pubkey
    |> option.map(fn(bits) { short_peer_id(sunset.bits_to_hex(bits)) })
  Relay(
    id: snap.id,
    host: parse_host(snap.label),
    raw_label: snap.label,
    state: parse_state(snap.state),
    attempt: snap.attempt,
    peer_id_short: peer_id_short,
    last_pong_at_ms: snap.last_pong_at_ms,
    last_rtt_ms: snap.last_rtt_ms,
  )
}

/// Filter `intents` to relays only and project to view-models.
/// Stable order: ascending by IntentId.
pub fn relays_for_view(
  intents: Dict(Float, IntentSnapshot),
) -> List(Relay) {
  intents
  |> dict.values()
  |> list.filter(fn(s) { is_relay_label(s.label) })
  |> list.sort(fn(a, b) {
    case a.id <. b.id, a.id >. b.id {
      True, _ -> order.Lt
      _, True -> order.Gt
      _, _ -> order.Eq
    }
  })
  |> list.map(from_intent)
}
```

- [ ] **Step 9.2: Create the test file**

```gleam
import gleam/dict
import gleam/option
import gleeunit/should
import sunset_web/domain.{
  Relay, RelayBackoff, RelayCancelled, RelayConnected, RelayConnecting,
}
import sunset_web/sunset.{IntentSnapshot}
import sunset_web/views/relays

pub fn is_relay_label_bare_hostname_test() {
  relays.is_relay_label("relay.sunset.chat") |> should.be_true()
}

pub fn is_relay_label_wss_test() {
  relays.is_relay_label("wss://relay.sunset.chat#x25519=ab") |> should.be_true()
}

pub fn is_relay_label_ws_test() {
  relays.is_relay_label("ws://127.0.0.1:8080") |> should.be_true()
}

pub fn is_relay_label_webrtc_test() {
  relays.is_relay_label("webrtc://abcdef#x25519=11") |> should.be_false()
}

pub fn parse_host_bare_hostname_test() {
  relays.parse_host("relay.sunset.chat") |> should.equal("relay.sunset.chat")
}

pub fn parse_host_wss_test() {
  relays.parse_host("wss://relay.sunset.chat") |> should.equal("relay.sunset.chat")
}

pub fn parse_host_with_port_test() {
  relays.parse_host("ws://127.0.0.1:8080") |> should.equal("127.0.0.1:8080")
}

pub fn parse_host_full_url_test() {
  relays.parse_host("wss://relay.sunset.chat:443/api?token=foo#x25519=abc")
  |> should.equal("relay.sunset.chat:443")
}

pub fn parse_state_known_test() {
  relays.parse_state("connected") |> should.equal(RelayConnected)
  relays.parse_state("connecting") |> should.equal(RelayConnecting)
  relays.parse_state("backoff") |> should.equal(RelayBackoff)
  relays.parse_state("cancelled") |> should.equal(RelayCancelled)
}

pub fn parse_state_unknown_falls_back_to_backoff_test() {
  relays.parse_state("eldritch_state") |> should.equal(RelayBackoff)
}

pub fn format_status_connected_test() {
  relays.format_status(RelayConnected, 0) |> should.equal("Connected")
}

pub fn format_status_connecting_test() {
  relays.format_status(RelayConnecting, 0) |> should.equal("Connecting")
}

pub fn format_status_backoff_zero_test() {
  relays.format_status(RelayBackoff, 0) |> should.equal("Backoff")
}

pub fn format_status_backoff_with_attempt_test() {
  relays.format_status(RelayBackoff, 3) |> should.equal("Backoff (attempt 3)")
}

pub fn format_status_cancelled_test() {
  relays.format_status(RelayCancelled, 7) |> should.equal("Cancelled")
}

pub fn format_rtt_present_test() {
  relays.format_rtt(option.Some(42)) |> should.equal("RTT 42 ms")
}

pub fn format_rtt_absent_test() {
  relays.format_rtt(option.None) |> should.equal("RTT —")
}

pub fn humanize_age_just_now_test() {
  relays.humanize_age(1000, option.Some(800)) |> should.equal("just now")
}

pub fn humanize_age_seconds_test() {
  relays.humanize_age(5500, option.Some(500)) |> should.equal("5s ago")
}

pub fn humanize_age_never_test() {
  relays.humanize_age(0, option.None) |> should.equal("never")
}

pub fn short_peer_id_short_unchanged_test() {
  relays.short_peer_id("abcdef") |> should.equal("abcdef")
}

pub fn short_peer_id_truncates_test() {
  relays.short_peer_id("0123456789abcdef0123456789abcdef")
  |> should.equal("01234567…89abcdef")
}

fn snap(id: Float, label: String, state: String) -> IntentSnapshot {
  IntentSnapshot(
    id: id,
    state: state,
    label: label,
    peer_pubkey: option.None,
    kind: option.None,
    attempt: 0,
    last_pong_at_ms: option.None,
    last_rtt_ms: option.None,
  )
}

pub fn from_intent_basic_test() {
  let s = snap(7.0, "relay.sunset.chat", "connected")
  let r = relays.from_intent(s)
  r.id |> should.equal(7.0)
  r.host |> should.equal("relay.sunset.chat")
  r.raw_label |> should.equal("relay.sunset.chat")
  r.state |> should.equal(RelayConnected)
  r.attempt |> should.equal(0)
  r.peer_id_short |> should.equal(option.None)
  r.last_pong_at_ms |> should.equal(option.None)
  r.last_rtt_ms |> should.equal(option.None)
}

pub fn relays_for_view_filters_webrtc_test() {
  let intents =
    dict.new()
    |> dict.insert(1.0, snap(1.0, "relay.sunset.chat", "connected"))
    |> dict.insert(2.0, snap(2.0, "webrtc://abc#x25519=11", "connected"))
    |> dict.insert(3.0, snap(3.0, "wss://other.example", "connecting"))
  let out = relays.relays_for_view(intents)
  // Two relays, sorted by id ascending.
  out |> list.length() |> should.equal(2)
  case out {
    [a, b] -> {
      a.id |> should.equal(1.0)
      b.id |> should.equal(3.0)
    }
    _ -> should.fail()
  }
}
```

Add `import gleam/list` at the top if missing.

- [ ] **Step 9.3: Run the Gleam tests**

Run: `cd web && nix develop --command gleam test 2>&1 | tail -30`
Expected: all `relays_test` cases pass. Existing `relay_status_pill_test` continues to pass. Other tests' state depends on Task 8 cleanup having landed cleanly.

- [ ] **Step 9.4: Commit**

```bash
git add web/src/sunset_web/views/relays.gleam web/test/sunset_web/views/relays_test.gleam
git commit -m "$(cat <<'EOF'
web: relays.gleam pure helpers + unit tests

is_relay_label (filters webrtc:// direct peers) / parse_host
(gleam/uri-backed with bare-hostname fallback) / parse_state
(unknown → RelayBackoff defensive fallback) / format_status (with
attempt-N rendering for Backoff) / format_rtt / humanize_age (mirrors
peer_status_popover) / short_peer_id / from_intent + relays_for_view
(filter + sort + project). View entry points come in the next change.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 10 — Render the Relays section in the channels rail (no popover yet)

**Files:**
- Modify: `web/src/sunset_web/views/relays.gleam` (add `rail_section` view)
- Modify: `web/src/sunset_web/views/channels.gleam` (replace placeholder with rail_section call)
- Modify: `web/src/sunset_web.gleam` (pass `relays` and `OpenRelayPopover` callback into `channels.view`; pass heartbeat URL param to `create_client`; add Msgs + Model field)

- [ ] **Step 10.1: Add Model + Msg + bootstrap wiring**

In `web/src/sunset_web.gleam`:

Add to `Model` (right after the `intents:` field or wherever popover state lives):

```gleam
    /// The IntentId of the relay whose popover is open, if any.
    /// Phone + desktop share this; placement is decided by `viewport`.
    relays_popover: option.Option(Float),
```

Initialise in the `Model(...)` constructor (where other defaults live):

```gleam
      relays_popover: option.None,
```

Add to `pub type Msg`:

```gleam
  OpenRelayPopover(Float)
  CloseRelayPopover
```

Update the `update` arm list:

```gleam
    OpenRelayPopover(id) -> #(
      Model(..model, relays_popover: option.Some(id)),
      effect.none(),
    )
    CloseRelayPopover -> #(
      Model(..model, relays_popover: option.None),
      effect.none(),
    )
```

Update the bootstrap effect that calls `sunset.create_client`. Find:

```gleam
sunset.create_client(seed, fn(client) { ... })
```

Replace with:

```gleam
let hb = sunset.heartbeat_interval_ms_from_url()
sunset.create_client(seed, hb, fn(client) { ... })
```

- [ ] **Step 10.2: Add `rail_section` to `relays.gleam`**

Add the lustre / theme / ui imports at the top of `web/src/sunset_web/views/relays.gleam` (Gleam imports go at the file top):

```gleam
import lustre/attribute
import lustre/element.{type Element}
import lustre/element/html
import lustre/event
import sunset_web/theme.{type Palette}
import sunset_web/ui
```

Then append the view functions to the file's end:

```gleam
pub fn rail_section(
  palette p: Palette,
  relays rs: List(Relay),
  on_open on_open: fn(Float) -> msg,
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
  r: Relay,
  on_open: fn(Float) -> msg,
) -> Element(msg) {
  html.button(
    [
      attribute.attribute("data-testid", "relay-row"),
      attribute.attribute("data-relay-host", r.host),
      attribute.attribute("data-relay-state", state_attr(r.state)),
      event.on_click(on_open(r.id)),
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
      html.span(
        [
          ui.css([
            #("flex", "1"),
            #("min-width", "0"),
            #("white-space", "nowrap"),
            #("overflow", "hidden"),
            #("text-overflow", "ellipsis"),
          ]),
        ],
        [html.text(r.host)],
      ),
    ],
  )
}

fn conn_dot(p: Palette, s: RelayConnState) -> Element(msg) {
  let c = case s {
    RelayConnected -> p.live
    RelayConnecting -> p.warn
    RelayBackoff -> p.warn
    RelayCancelled -> p.text_faint
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

fn state_attr(s: RelayConnState) -> String {
  case s {
    RelayConnected -> "connected"
    RelayConnecting -> "connecting"
    RelayBackoff -> "backoff"
    RelayCancelled -> "cancelled"
  }
}
```

- [ ] **Step 10.3: Wire `rail_section` into `channels.gleam`**

In `web/src/sunset_web/views/channels.gleam`, add the import:

```gleam
import sunset_web/views/relays as relays_view
```

Add to the `view` function's labeled-arg list (alongside `voice_popover_open`):

```gleam
  relays relays: List(domain.Relay),
  on_open_relay on_open_relay: fn(Float) -> msg,
```

Replace the placeholder fragment that previously sat where the Bridges section was, with:

```gleam
          relays_view.rail_section(
            palette: p,
            relays: relays,
            on_open: on_open_relay,
          ),
```

- [ ] **Step 10.4: Pass `relays` from the shell**

In `web/src/sunset_web.gleam`, locate every `channels.view(...)` call site (search `rg -n 'channels.view(' web/src/sunset_web.gleam`). Add to each call:

```gleam
        relays: relays_view.relays_for_view(model.intents),
        on_open_relay: OpenRelayPopover,
```

Add the import at the top (mirrors the existing `views/...` imports):

```gleam
import sunset_web/views/relays as relays_view
```

- [ ] **Step 10.5: Build and verify in a browser**

Run: `cd web && nix develop --command gleam build 2>&1 | tail -10`
Expected: clean.

Per CLAUDE.md, verify UI changes in a browser before claiming complete. Start the dev server (find the script via `cat web/package.json | grep -A1 scripts`); open the page with a relay configured (e.g. `?relay=ws://127.0.0.1:PORT#x25519=...` against a local relay you spawn out-of-band, or just rely on the default `relay.sunset.chat` resolution if internet is available). Confirm the "Relays" section appears under the channels rail. Click does nothing yet — popover lands in Task 11.

If no browser verification is possible from your shell, say so explicitly and proceed to Task 11.

- [ ] **Step 10.6: Commit**

```bash
git add web/src/sunset_web/views/relays.gleam web/src/sunset_web/views/channels.gleam web/src/sunset_web.gleam
git commit -m "$(cat <<'EOF'
web: render Relays section in channels rail

Replaces the dummy Bridges slot with a live list of relays sourced
from the supervisor's IntentSnapshot stream (filtered to non-webrtc
labels). Status dot colours mirror ConnStatus (live / warn / faint).
Click handler dispatches OpenRelayPopover; popover wiring lands next.

Bootstrap also threads the new ?heartbeat_interval_ms URL param into
Client::new for e2e use.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 11 — Popover view + desktop floating / phone bottom-sheet wiring

**Files:**
- Modify: `web/src/sunset_web/views/relays.gleam` (add `popover` view + `Placement`)
- Modify: `web/src/sunset_web.gleam` (mount overlays alongside `peer_status_popover_overlay`)

- [ ] **Step 11.1: Add the popover view**

Append to `web/src/sunset_web/views/relays.gleam`:

```gleam
pub type Placement {
  Floating
  InSheet
}

pub fn popover(
  palette p: Palette,
  relay r: Relay,
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
        info_row(
          p,
          "relay-popover-heard-from",
          "heard from " <> humanize_age(now, r.last_pong_at_ms),
        ),
        info_row(p, "relay-popover-rtt", format_rtt(r.last_rtt_ms)),
        mono_row(p, "relay-popover-label", r.raw_label),
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

fn status_pill(p: Palette, state: RelayConnState, label: String) -> Element(msg) {
  let bg = case state {
    RelayConnected -> p.live
    RelayConnecting -> p.warn
    RelayBackoff -> p.warn
    RelayCancelled -> p.text_faint
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

- [ ] **Step 11.2: Mount overlays in `sunset_web.gleam`**

Locate `peer_status_popover_overlay` (search `rg -n 'peer_status_popover_overlay\|peer_status_sheet_el' web/src/sunset_web.gleam`).

For desktop, add a sibling function:

```gleam
fn relay_popover_overlay(
  palette: theme.Palette,
  model: Model,
) -> Element(Msg) {
  case model.viewport, model.relays_popover {
    domain.Desktop, option.Some(id) -> {
      let rs = relays_view.relays_for_view(model.intents)
      case list.find(rs, fn(r) { r.id == id }) {
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
    }
    _, _ -> element.fragment([])
  }
}
```

Mount it where `peer_status_popover_overlay(palette, model, state)` is called — add immediately after:

```gleam
relay_popover_overlay(palette, model),
```

For phone, define a sibling sheet element next to `peer_status_sheet_el`:

```gleam
let relay_sheet_el = case model.viewport, model.relays_popover {
  domain.Phone, option.Some(id) -> {
    let rs = relays_view.relays_for_view(model.intents)
    case list.find(rs, fn(r) { r.id == id }) {
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
  }
  _, _ -> element.fragment([])
}
```

Mount `relay_sheet_el` in the same render block as `peer_status_sheet_el`.

If `model.now_ms` is named differently, search `rg -n 'now_ms\|NowTick' web/src/sunset_web.gleam` and substitute.

- [ ] **Step 11.3: Build and verify**

Run: `cd web && nix develop --command gleam build 2>&1 | tail -10`
Expected: clean.

Browser smoke (per CLAUDE.md): open the dev server, click a Relays row. Desktop: a floating card opens top-right with hostname / status / "heard from" / RTT / label. Phone (DevTools 390×844): the same content slides up as a bottom sheet. Close via × works in both. If no browser is available, skip and rely on the e2e test in Task 12.

- [ ] **Step 11.4: Commit**

```bash
git add web/src/sunset_web/views/relays.gleam web/src/sunset_web.gleam
git commit -m "$(cat <<'EOF'
web: relay popover (desktop floating + phone bottom sheet)

Click a Relays row to see hostname header, status pill, last
heartbeat (live-ticking via the existing now_ms ticker), RTT, full
label (mono, selectable), and short peer id (when known).
Placement mirrors peer_status_popover (Floating | InSheet) and
mounts inside the existing bottom_sheet host on phone.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 12 — Playwright e2e

**Files:**
- Create: `web/e2e/relays.spec.js`

- [ ] **Step 12.1: Author the spec**

Mirror the spawn-relay pattern from `web/e2e/peer_status_popover.spec.js`:

```javascript
// Acceptance test for the Relays rail and popover.
//
// Spawns a real sunset-relay, points one browser at it, and asserts:
//   * Relays section + row appear with the correct hostname.
//   * Row state attribute reaches "connected".
//   * Click opens the popover (hostname, status, heard-from, RTT, label).
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
    popover.locator('[data-testid="relay-popover-label"]'),
  ).toContainText(relayAddress);

  // Within ~6 s (3 × heartbeat_interval_ms=2000) we should see RTT.
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

  // On phone the channels rail may live inside a drawer; open it
  // through whatever opener the existing UI uses. The selector below
  // is a guess — search the codebase for the actual phone-side
  // channels-drawer testid before assuming the if-block always runs.
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
  // The popover's `data-testid="bottom-sheet"` ancestor proves we're
  // in the InSheet placement, not the Floating desktop card.
  const sheetAncestor = popover.locator(
    'xpath=ancestor::*[@data-testid="bottom-sheet"]',
  );
  await expect(sheetAncestor).toHaveCount(1);

  await ctx.close();
});
```

If the phone-channels-drawer test-id is named differently, search:

```bash
nix develop --command rg -n 'phone-open\|ChannelsDrawer\|channels-drawer\|drawer.*open' web/src/
```

and substitute. If the channels rail is always reachable on phone without an explicit opener, the `if (await ...count())` block becomes a harmless no-op.

- [ ] **Step 12.2: Run the new spec**

Run: `cd web && nix develop --command npx playwright test relays.spec.js 2>&1 | tail -40`
Expected: both tests PASS. If the desktop test fails on the RTT assertion within 8 s, double-check (a) the heartbeat URL param made it through (Task 7 + Task 10), (b) `Client::new` actually applies it (Task 6), and (c) the supervisor's `handle_engine_event` fires `PongObserved` and propagates to the snapshot (Task 5). Use the systematic-debugging skill — never paper over a real bug with a longer timeout. The test asserts a contract a real user would notice.

- [ ] **Step 12.3: Run the full e2e suite to catch regressions**

Run: `cd web && nix develop --command npx playwright test 2>&1 | tail -30`
Expected: no regressions. If any spec fails because it relied on the now-removed Bridge fixture data, fix it. If `relay_deploy.spec.js` fails on a timing issue introduced by the heartbeat change, investigate root cause — don't loosen the test.

- [ ] **Step 12.4: Commit**

```bash
git add web/e2e/relays.spec.js
git commit -m "$(cat <<'EOF'
web: e2e for Relays rail + popover (desktop + phone bottom sheet)

Spawns a real sunset-relay, asserts:
  * Relays section + row materialise with the right hostname.
  * Row state reaches "connected".
  * Popover opens with status pill, full label, live RTT and
    heard-from age (the latter two driven by the new
    heartbeat_interval_ms=2000 URL param so the test fits under 15 s).
  * On phone viewport the popover lives inside the bottom-sheet host.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 13 — Cross-cutting verification

**Files:** none modified — verification only.

- [ ] **Step 13.1: Full Rust workspace test suite**

Run: `nix develop --command cargo test --workspace --all-features 2>&1 | tail -20`
Expected: all PASS.

- [ ] **Step 13.2: Workspace clippy with no-suppressions policy**

Run: `nix develop --command cargo clippy --workspace --all-features --all-targets -- -D warnings 2>&1 | tail -30`
Expected: clean. If clippy flags anything new from this change, fix at source — `#[allow]` / `#[expect]` are forbidden per CLAUDE.md.

Run: `nix develop --command bash scripts/check-no-clippy-allow.sh 2>&1 | tail`
Expected: empty.

- [ ] **Step 13.3: Format check**

Run: `nix develop --command cargo fmt --all --check 2>&1 | tail`
Expected: clean. If diffs, run `cargo fmt --all` and fold into a small "chore: cargo fmt" commit.

- [ ] **Step 13.4: Gleam build + tests**

Run: `cd web && nix develop --command gleam build 2>&1 | tail -10`
Run: `cd web && nix develop --command gleam test 2>&1 | tail -20`
Expected: both clean.

- [ ] **Step 13.5: Full Playwright suite**

Run: `cd web && nix develop --command npx playwright test 2>&1 | tail -30`
Expected: all PASS.

- [ ] **Step 13.6: Mobile + desktop visual smoke**

CLAUDE.md requires browser verification for UI changes. With a relay configured, exercise the golden path on both desktop and DevTools mobile (390×844): the Relays section is visible, click opens a popover with live metrics, close works, the desktop popover floats top-right while the phone version is the bottom sheet. If no browser is available from your shell, say so explicitly and lean on the e2e tests as the verification.

- [ ] **Step 13.7: Wrap-up commit if any verification work produced changes; otherwise nothing to commit.**

---

## Spec coverage cross-check

| Spec section | Tasks |
|---|---|
| §1 PongObserved + IntentSnapshot fields | T1, T2, T3, T4, T5 |
| §1 disconnect-preserves-liveness | T5 (`disconnect_preserves_last_pong_and_rtt`) |
| §1 wire format unchanged | (no protocol change; only InboundEvent/EngineEvent) |
| §2 wasm bridge enrichment + heartbeat constructor | T6 |
| §3 FFI: heartbeat URL param + IntentSnapshot extension | T7 |
| §3 Domain `Relay` + `RelayConnState`; remove Bridge/Minecraft | T8 |
| §3 Pure helpers (`is_relay_label`, `parse_host`, …) | T9 |
| §3 View `rail_section` + `popover` (Floating/InSheet) | T10, T11 |
| §3 Channels-rail integration | T10 |
| §3 Mobile bottom sheet | T11, T12 (phone test) |
| §3 `data-testid` hooks | T10 (`relays-section`, `relay-row`, `data-relay-host/state`), T11 (`relay-popover*`) |
| §4 Fixture cleanup | T8 |
| Tests — Rust unit | T2, T3, T5 |
| Tests — Gleam pure | T9 |
| Tests — Playwright e2e | T12 |
| Out-of-scope items NOT implemented | (per spec — confirmed not in any task) |
