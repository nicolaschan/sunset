# Relay Audio Fallback — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:subagent-driven-development. Steps use `- [ ]`. Run cargo via the flake toolchain (`/nix/store/...-rust-default-1.95.0/bin` on PATH). After each task: `cargo fmt --all`, `cargo clippy --workspace --all-features --all-targets -- -D warnings`, the task's tests, `scripts/check-no-clippy-allow.sh`, then commit. NEVER `#[allow(clippy::...)]`. Read the spec (`docs/superpowers/specs/2026-06-01-relay-audio-fallback-design.md`) and `~/.claude/CLAUDE.md` + `./CLAUDE.md` first.

**Goal:** Carry voice audio through a relay when two peers can't form a direct WebRTC link; prefer WebRTC and keep retrying it; switch both ways.

**Architecture:** Two layers. Data plane: the engine re-forwards ephemeral datagrams via the same `forward_targets` durable uses, deduped by a per-`(sender,name)` in-memory seq HWM; the authoritative seq lives on the `SignedDatagram` envelope. Control plane: voice subscribes per call participant via `subscribe_via`, provider derived (direct peer vs the sole `Primary` relay) from `current_peers()` kind, convergent on engine peer-events.

**Tech Stack:** Rust workspace (`?Send`/WASM data plane), postcard wire, Gleam/web + Playwright.

**Wire bump touches FOUR frozen vectors** (verified): `datagram_payload_frozen_vector`, `datagram_payload_excludes_signature_field` (`store/src/canonical.rs`); `voice_packet_frame_postcard_frozen_vector` (`voice/src/packet.rs`); `ephemeral_delivery_frozen_vector` (`sync/src/message.rs:157`). Plus the literal in `ephemeral_delivery_postcard_roundtrip` (`message.rs:138`) needs the new field. Grep `frozen_vector` to confirm exactly these move.

---

## Task 1: SignedDatagram gains an authenticated `seq` (+ all 4 frozen vectors)

**Files:** Modify `store/src/types.rs`, `store/src/canonical.rs`, `sync/src/message.rs`; fix every `SignedDatagram { .. }` literal (compiler-listed). Tests in `canonical.rs` + `message.rs`.

- [ ] **Step 1: Failing test** in `canonical.rs` tests (it uses `VerifyingKey::new(Bytes::from_static(..))` inline — no `vk()` import there):
```rust
#[test]
fn datagram_signing_payload_covers_seq() {
    let mk = |seq| SignedDatagram {
        verifying_key: VerifyingKey::new(Bytes::from_static(b"alice")),
        name: Bytes::from_static(b"voice/r/alice"), payload: Bytes::from_static(b"hi"),
        seq, signature: Bytes::new(),
    };
    assert_ne!(datagram_signing_payload(&mk(7)), datagram_signing_payload(&mk(8)));
}
```
- [ ] **Step 2: Run, expect FAIL** (`no field seq`): `cargo test -p sunset-store --lib canonical`.
- [ ] **Step 3: Implement.** `types.rs`: add `pub seq: u64,` to `SignedDatagram` between `payload` and `signature` (frozen order `verifying_key, name, payload, seq, signature`). `canonical.rs`: add `seq: &'a u64` to `UnsignedDatagramRef` after `payload` + to the value. Fix all `SignedDatagram{..}` literals (tests pass `seq: 0`).
- [ ] **Step 4: Run; expect the new test PASS** and FOUR frozen tests FAIL. Regenerate each by copying the new `actual` hex into the constant, and update `datagram_payload_excludes_signature_field`'s covered set to include `seq`. The four: `datagram_payload_frozen_vector` + `datagram_payload_excludes_signature_field` (canonical.rs), `ephemeral_delivery_frozen_vector` (message.rs:157), and add `seq` to the `SignedDatagram{..}` literal in `ephemeral_delivery_postcard_roundtrip` (message.rs:138). (The voice `voice_packet_frame_postcard_frozen_vector` is regenerated in Task 2 when `Frame.seq` is removed.) Above each regenerated constant: `// Regenerated 2026-06-01: deliberate pre-1.0 ephemeral wire bump (added SignedDatagram.seq).`
- [ ] **Step 5:** `cargo build --workspace` compiles; `cargo test -p sunset-store -p sunset-sync --all-features` green. `grep -rn frozen_vector crates/ | wc -l` — confirm only the four moved.
- [ ] **Step 6: Commit** `store: add authenticated seq to SignedDatagram envelope (pre-1.0 ephemeral wire bump)`.

---

## Task 2: Voice — per-stream envelope seq through the Bus, remove payload seq (merged seam)

> Tasks "bus signature" and "voice seq" are merged: the heartbeat counter doesn't exist until voice changes, so splitting risks a second seq source. One compiling commit.

**Files:** Modify `core/src/bus.rs` (`Bus::publish_ephemeral`, `BusImpl`), `voice/src/runtime/dyn_bus.rs` + `dyn_bus_impl.rs` (`DynBus::publish_ephemeral`), `voice/src/packet.rs` (remove `Frame.seq` + regen vector), `voice/src/runtime/mod.rs` (frame counter→envelope), `voice/src/runtime/heartbeat.rs` (heartbeat counter + name `/hb`), `voice/src/runtime/subscribe.rs` (envelope seq + Option dedup), `voice/src/runtime/traits.rs` (`FrameSink::deliver` seq from envelope — type already `u32`), and **every** `publish_ephemeral` call site (grep — `core/src/bus.rs:233,287`, `core/tests/bus_integration.rs:119`, `core/tests/voice_two_peer.rs:122`, `core/tests/liveness_with_bus.rs:145`, `voice/.../dyn_bus_impl.rs:29,125`, `voice` frame/heartbeat). Tests in `core/src/bus.rs` + `voice` + fix `voice/tests/runtime_integration.rs`.

- [ ] **Step 1: Failing tests.**
  - `core/src/bus.rs` (reuse `make_bus` harness, loopback precedent bus.rs:223): `publish_ephemeral(name, 42, payload)` → received `SignedDatagram.seq == 42`.
  - voice: `frame_and_heartbeat_distinct_names` (frame→`voice/{fp}/{pk}`, heartbeat→`voice/{fp}/{pk}/hb`); `receiver_delivers_first_frame_seq_0_once` (envelope seq=0 first frame is delivered, NOT dropped); `receiver_dedups_same_sender_seq` (two datagrams same `(sender,seq)` → one `deliver`).
- [ ] **Step 2: Run, expect FAIL.**
- [ ] **Step 3: Implement.**
  - `bus.rs`: `Bus::publish_ephemeral(&self, name: Bytes, seq: u64, payload: Bytes)`; `BusImpl` sets `seq` on the assembled datagram. Mirror `DynBus`/`DynBusImpl`. Update ALL call sites to pass a seq.
  - `packet.rs`: remove `seq` from `VoicePacket::Frame`. Regenerate `voice_packet_frame_postcard_frozen_vector` (new hex + wire-bump comment).
  - `mod.rs`: keep `inner.seq` (frame counter), stamp on envelope (`publish_ephemeral(name, seq, payload)`), stop stamping the packet.
  - `heartbeat.rs`: add a heartbeat counter (RefCell<u64> in the heartbeat task/`RuntimeInner`); name `format!("voice/{room_fp}/{sender_pk}/hb")`; `publish_ephemeral(hb_name, hb_seq, payload)`.
  - `subscribe.rs`: read `seq` from `SignedDatagram.seq` (the datagram is in scope at the match arm); dedup with **Option** (`match last.get(sender) { Some(&h) if seq <= h => return, _ => { last.insert(sender, seq); } }` — never `unwrap_or(0)`); pass envelope seq into `deliver`.
- [ ] **Step 4: Fix fixtures.** `voice/tests/runtime_integration.rs`: the `SignedDatagram{..}` literals (≈lines 88,529,617,682) need `seq`; multi-frame injections (≈514,604,766) must stamp **strictly increasing envelope seqs**; rewrite the line-≈556 assertion to read the envelope seq.
- [ ] **Step 5: Run, expect PASS** (`cargo test -p sunset-core -p sunset-voice --all-features`).
- [ ] **Step 6: fmt + clippy + Commit** `voice: per-stream envelope seq (frame/heartbeat distinct streams); receiver Option-dedup; remove payload Frame.seq`.

---

## Task 3: Data plane — relay re-forwards ephemeral, deduped, counted-on-fanout

**Files:** Modify `sync/src/engine.rs` (`EngineState` fields; `handle_ephemeral_delivery`; `ephemeral_forwarded()` accessor; HWM prune in `drop_peer_session`). Tests: engine.rs test module (explicit-kind harness) + `sync/tests/relay_ephemeral.rs` (TestNetwork star).

- [ ] **Step 1: Failing unit tests** (build via `TestNetwork` + per-engine `build_engine` + `spawn_local`, the real pattern in `ephemeral_two_peer.rs`; to get explicit kinds, **inject `peer_sessions` with explicit `TransportKind`** as engine.rs:2286 does — the kind is the unit under test, TestTransport's `kind()` is `Unknown`):
  - `ephemeral_forwarded_only_when_fanned_out`: R has B armed (interest for `voice/{A}`); deliver A's datagram seq=0 to R → forwarded to B, NOT echoed to A (`*peer == from` skip), `R.ephemeral_forwarded() == 1`. Then with NO armed peer, deliver A seq=1 → no fanout, **`ephemeral_forwarded()` stays 1** (the load-bearing "stops rising" property).
  - `ephemeral_hwm_drops_replays_keeps_first_seq_0`: first datagram seq=0 IS forwarded (Option gate, not `<=0`); same `(sender,name,0)` again → dropped; seq=1 → forwarded; seq=0 after → dropped.
- [ ] **Step 2: Run, expect FAIL.**
- [ ] **Step 3: Implement** `handle_ephemeral_delivery(from, datagram)` after the sig check:
```rust
let key = (datagram.verifying_key.clone(), datagram.name.clone());
let mut st = self.state.lock().await;
match st.ephemeral_hwm.get(&key) {            // Option: distinguishes "none" from "0"
    Some(&h) if datagram.seq <= h => return,
    _ => {}
}
let msg = SyncMessage::EphemeralDelivery { datagram: datagram.clone() };
let mut fanned = 0u64;
for (peer, session) in crate::routing::forward_targets(&st.peer_sessions, &datagram.verifying_key, &datagram.name) {
    if *peer == from { continue; }
    let _ = session.tx.send(msg.clone());
    fanned += 1;
}
st.ephemeral_hwm.insert(key, datagram.seq);   // advance HWM regardless (dedup), but...
if fanned > 0 { st.ephemeral_forwarded += fanned; }   // ...count only real fan-out
drop(st);
self.dispatch_ephemeral_local(&datagram).await;   // local subscribers (lock released first)
```
  - `EngineState`: `ephemeral_hwm: HashMap<(VerifyingKey, Bytes), u64>`, `ephemeral_forwarded: u64`.
  - `pub async fn ephemeral_forwarded(&self) -> u64` (lock, read).
  - In `drop_peer_session`, retain only HWM keys whose sender `!=` removed peer.
- [ ] **Step 4: Run, expect PASS.**
- [ ] **Step 5: Integration** `sync/tests/relay_ephemeral.rs`: 3 engines A/R/B via TestNetwork, A–B not connected (only A–R, B–R), B `subscribe_via(NamePrefix("voice/{A}"), provider=R, ephemeral policy)`; assert B receives A's ephemeral, `!A.current_peers().contains(B)` (R present is fine), `R.ephemeral_forwarded() >= 1`.
- [ ] **Step 6: fmt + clippy + `cargo test -p sunset-sync --all-features` + Commit** `sync: relay re-forwards ephemeral datagrams (HWM dedup, counted only on fan-out, source-excluded)`.

---

## Task 4: Relay exposes `ephemeral_forwarded` on the JSON identity route

**Files:** Modify `relay/src/bridge.rs` (`IdentitySnapshot` field), `relay/src/snapshot.rs` (`build_identity_snapshot` takes engine), `relay/src/app.rs`/route (`render_identity` JSON). The JSON `/` route serves `IdentitySnapshot` (app.rs:30,88 `application/json`); `/dashboard` is plaintext — the e2e fetches `/`.

- [ ] **Step 1: Failing test** — `build_identity_snapshot(.., engine)` includes `ephemeral_forwarded` from `engine.ephemeral_forwarded()`.
- [ ] **Step 2: Run, expect FAIL.**
- [ ] **Step 3: Implement.** Add `pub ephemeral_forwarded: u64` to `IdentitySnapshot`; thread `ctx.engine` (available at relay.rs:423 identity arm) into `build_identity_snapshot`; populate via the accessor; ensure `render_identity` serializes it. (Optionally also add to `DashboardSnapshot` plaintext line.)
- [ ] **Step 4: Run, expect PASS.**
- [ ] **Step 5: fmt + clippy + `cargo test -p sunset-relay` + `scripts/check-desktop-lints.sh` (if relay touched desktop mirror — it doesn't) + Commit** `relay: expose ephemeral_forwarded on the JSON identity route`.

---

## Task 5: Bus/DynBus routing+observation seam (delegation is the new part)

> `subscribe_via`/`unsubscribe_via`/`current_peers`/`subscribe_engine_events`/`subscribe_ephemeral` already exist as **public `SyncEngine` methods**. The new work is **delegation**: `BusImpl.engine` is private (bus.rs:64), so voice's `DynBus` impl cannot reach them — they must be exposed on the `Bus` trait (impl on `BusImpl`, which sees its engine), then on `DynBus`.

**Files:** Modify `core/src/bus.rs` (`Bus` trait + `BusImpl`), `voice/src/runtime/dyn_bus.rs` + `dyn_bus_impl.rs`. Re-export `TransportKind`, `Filter`, `SubscriptionPolicy`, `PeerId` paths into voice. Tests: voice (trait object over a real engine).

- [ ] **Step 1: Failing test** — over a `TestNetwork` engine wrapped in `DynBusImpl`: `current_peers()` returns `(PeerId, TransportKind)`; `subscribe_via`/`unsubscribe_via` callable; `subscribe_ephemeral_local(filter)` yields a stream with **no** BroadcastIntent (assert: a remote peer is NOT armed by it — i.e. `engine.subscriptions_snapshot()` does not gain an entry).
- [ ] **Step 2: Run, expect FAIL.**
- [ ] **Step 3: Implement.** Add to `Bus` (`?Send`) + `BusImpl` (delegating to `self.engine`): `subscribe_via(Filter, PeerId, SubscriptionPolicy)`, `unsubscribe_via(Filter, PeerId)`, `current_peers() -> Vec<(PeerId, TransportKind)>`, `subscribe_engine_events() -> mpsc::UnboundedReceiver<EngineEvent>`, and `subscribe_ephemeral_local(Filter) -> LocalBoxStream<SignedDatagram>` (wraps `SyncEngine::subscribe_ephemeral`, the in-process channel, **no** intent). Mirror all on `DynBus` + `DynBusImpl`. All `?Send`, no new bound.
- [ ] **Step 4: Run, expect PASS.**
- [ ] **Step 5: fmt + clippy + Commit** `core+voice: expose routing/observation + intent-free local ephemeral subscribe on Bus/DynBus`.

---

## Task 6: ICE config seam (so the e2e can block WebRTC at the environment)

> Verified: `Client::new(seed, heartbeat_interval_ms)` HARDCODES `vec!["stun:stun.l.google.com:19302"]` (client.rs:135) into `WebRtcRawTransport::new(.., ice_urls)`. There is NO knob today. The e2e needs one (empty/black-hole ICE = genuine ICE failure). This is real production API work.

**Files:** Modify `sunset-web-wasm/src/client.rs` (`Client::new` gains `ice_urls`), `web/.../sunset.ffi.mjs` (`createClient` passes it), the Gleam call site. Default = today's STUN. Test: Rust propagation test.

- [ ] **Step 1: Failing test** — `Client::new(seed, hb, ice)` stores `ice` and passes it to `WebRtcRawTransport::new` (assert via a seam/getter or a constructor that records it).
- [ ] **Step 2: Run, expect FAIL.**
- [ ] **Step 3: Implement.** Add `ice_urls: Vec<String>` param to `Client::new` (or a `#[wasm_bindgen]` setter); thread to `WebRtcRawTransport::new`; default to the STUN value when JS passes empty? No — JS always passes; default lives in the Gleam/FFI call. Update `createClient(seed, hbMs, iceUrls, callback)` in `sunset.ffi.mjs:61` and the Gleam caller to pass the current STUN list by default and an empty/black-hole list when a test config requests it (a URL query param the app reads).
- [ ] **Step 4: Run, expect PASS** (Rust propagation test) + `nix build .#web` compiles Gleam.
- [ ] **Step 5: fmt + clippy + Commit** `web: thread ICE server config through Client::new + FFI (default STUN)`.

---

## Task 7: Control plane — per-peer voice provider convergence

**Files:** Create `voice/src/runtime/voice_provider.rs`; modify `voice/src/runtime/mod.rs`/`state.rs` (spawn; replace broad arming); `voice/src/runtime/subscribe.rs` (use `subscribe_ephemeral_local` for receive). Tests: voice (explicit-kind harness as Task 3).

- [ ] **Step 1: Failing tests** (inject `peer_sessions`/`current_peers` with explicit `TransportKind`, or a MultiTransport star):
  - `provider_direct_when_secondary`: `(A,Secondary)` present → arms `subscribe_via(voice/{A}, provider=A)`, no relay sub for A.
  - `provider_relay_when_no_direct`: only `(relay,Primary)` → arms `subscribe_via(voice/{A}, provider=relay)`.
  - `convergence_via_consequence`: drop `(A,Secondary)` → reconverge to relay → A's published ephemeral now reaches B and `R.ephemeral_forwarded()` rises; re-add Secondary → reconverge to A, R counter flat.
  - `relay_only_participant_sets_in_call`: a participant reachable only via relay — its `/hb` heartbeats reach `membership_liveness`, `in_call` set.
- [ ] **Step 2: Run, expect FAIL.**
- [ ] **Step 3: Implement** `voice_provider.rs`: subscribe to `bus.subscribe_engine_events()`; on each event recompute, for every roster participant A: `desired = if current_peers contains (A,Secondary) { A } else { sole Primary peer }`; if changed, `unsubscribe_via(voice/{A}, old)` then `subscribe_via(NamePrefix("voice/{fp}/{a_pk}"), desired, ephemeral_policy)` — convergent, idempotent, re-asserted every event. In `subscribe.rs` swap `subscribe_voice_prefix` (BroadcastIntent) for `subscribe_ephemeral_local(NamePrefix("voice/{fp}/"))` (local decode/membership, no remote arming). Roster from existing presence/membership. Filter for A covers frame + `/hb` (shared prefix). Spawn the component from `mod.rs` alongside `auto_connect`.
- [ ] **Step 4: Run, expect PASS.**
- [ ] **Step 5: fmt + clippy + `cargo test -p sunset-voice --all-features` + Commit** `voice: per-peer provider — prefer direct, fall back to relay, convergent on engine events`.

---

## Task 8: Per-frame inbound provenance (`via`: Local | Direct | Relay)

**Files:** Modify `core/src/bus.rs` (`BusEvent::Ephemeral` gains `via`), `sync/src/engine.rs` (both `dispatch_ephemeral_local` callers set it: inbound from `peer_sessions[from].kind`; local-publish = `Local`), `voice/src/runtime/subscribe.rs` (pass via to deliver), `voice/src/runtime/traits.rs` (`FrameSink::deliver(peer, seq, pcm, via)`), `sunset-web-wasm/src/voice/test_hooks.rs` (`RecordedFrame.via`), `sunset-web-wasm/src/voice/mod.rs:270` (recorded_frames JSON includes `via`). Note: spans `sunset-voice` AND `sunset-web-wasm`.

- [ ] **Step 1: Failing test** (voice/sync): feed `handle_ephemeral_delivery` one `EphemeralDelivery` whose `from` has a `Secondary` session and one whose `from` has a `Primary` session; assert the recorded `via == Direct` / `Relay` respectively. **The test must source `via` from the inbound session kind, and FORBID re-deriving it from current connectivity** (a switchover-time fake would mislabel).
- [ ] **Step 2: Run, expect FAIL.**
- [ ] **Step 3: Implement.** `enum FrameVia { Local, Direct, Relay }` (Direct=inbound Secondary, Relay=inbound Primary). `dispatch_ephemeral_local` gains a `via: FrameVia` arg: `handle_ephemeral_delivery` passes `peer_sessions[from].kind → Direct/Relay`; `publish_ephemeral` passes `Local`. Thread through `BusEvent::Ephemeral{datagram, via}` (fix the ~9 match sites; non-voice ones ignore `via`), the DynBus ephemeral item, `subscribe.rs` → `deliver(.., via)`, `RecordedFrame.via`, recorded_frames JSON.
- [ ] **Step 4: Run, expect PASS.**
- [ ] **Step 5: fmt + clippy + Commit** `voice+web-wasm: tag received frames with inbound transport provenance (local/direct/relay)`.

---

## Task 9: E2E — `voice_relay_fallback.spec.js` (honesty-critical)

**Files:** Create `web/e2e/voice_relay_fallback.spec.js`; reuse `web/e2e/helpers/voice.js`, `client.intents()` (IntentSnapshotJs: `kind`, `state`, `attempt` — intent.rs:22,34; precedent `voice_rejoin_receives.spec.js`), the recorded-frames `via` tag, and the relay `/` JSON `ephemeral_forwarded`.

- [ ] **Step 1: Write the spec.** Two real clients A,B + real `sunset-relay`; both constructed with **empty/black-hole ICE** (via Task 6 knob) so direct WebRTC genuinely fails. Inject PCM at A.
  - **Relay carried it (positive):** `fetch('/')` the relay JSON, record `ephemeral_forwarded` baseline; after injection assert it rose ≥ N; **and** B's recorded frames ≥ N tagged `via:"relay"`.
  - **WebRTC genuinely failed, not bypassed:** assert `client.intents()` has the `webrtc://B` intent with rising `attempt` / `state:"backoff"` over the window (supervisor retrying); assert NO intent for B is `kind:"secondary"` & `state:"connected"`. (Note in a comment: `data-voice-connected` is liveness-derived and TRUE over the relay — it is NOT a path discriminator.)
  - **Prefer-WebRTC + retry:** reconfigure ICE to reachable (reload B with a normal ICE config, or the test env's host candidates) → assert a `kind:"secondary","state":"connected"` intent for B forms, newly-recorded frames flip to `via:"direct"` with count climbing, and the relay `ephemeral_forwarded` **stops rising** for the pair (sample twice, ≥2s apart, equal). Then re-apply the block → `via:"relay"` resumes — both directions.
- [ ] **Step 2: Run** `nix run .#web-test-voice -- e2e/voice_relay_fallback.spec.js`; iterate to green.
- [ ] **Step 3: Commit** `e2e: voice_relay_fallback — audio crosses the relay when WebRTC blocked; prefers WebRTC when available`.

> **HONESTY GATES:** no `wait_for` on engine internals; no inspector gating a user action; relay-path proof = server forward counter + `via` tag + intents() backoff/secondary (all first-class), never absence-of-Secondary via `window.__`; block is environmental (empty ICE), dial machinery runs and FAILS (asserted via intents().attempt). The on-wire `SubscriptionEntry::Withdrawn` is asserted in a **Rust** test (`subscriptions_snapshot()`), not Playwright.

---

## Task 10: Full verification + flake gate + PR

- [ ] `cargo test --workspace --all-features` green; clippy/fmt clean; `check-no-clippy-allow.sh` + `check-desktop-lints.sh` pass; `grep -rn frozen_vector` = the four expected.
- [ ] Build + run full voice e2e suite ISOLATED; then `--repeat-each=200 --workers=4` on `voice_relay_fallback.spec.js` and `voice_three_way.spec.js`, **isolated** (no concurrent runs — concurrency causes false flakes), 0 failures.
- [ ] Open PR (draft→ready), push, confirm CI green **3× in a row**. Red → systematic-debugging; a pre-existing master flake (`relay_504`) is not a regression (confirm isolated). PR body: summarize the two layers, the wire bump, and the e2e honesty signals.

---

## Self-review (spec coverage)

Wire bump (4 vectors): T1+T2. One seq, no denormalization: T2 (payload seq removed). Re-forward = durable shape, no role gate, HWM, counted-on-fanout, source-excluded: T3. Counter JSON: T3+T4. Bus/DynBus seam (delegation + intent-free local): T5. ICE knob: T6. Provider convergence + membership preserved: T7. Provenance enum (loopback=Local): T8. Honest e2e (forward counter + via tag + intents backoff/secondary; env ICE block; Withdrawn→Rust): T9. 3× green + isolated stress: T10. WASM/?Send, clippy-no-suppress, nix: per-task.
