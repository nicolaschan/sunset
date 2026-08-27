# Voice: `LevelSink` — peer + self loudness in sunset-voice

**Date:** 2026-05-12
**Scope:** Move per-peer and self mic loudness (currently a JS-side EMA + decay timer in `web/src/sunset_web/voice.ffi.mjs`) into `sunset-voice`. Expose it via a new `LevelSink` trait that mirrors `PeerStateSink`'s shape but fires at a continuous cadence instead of on state transitions.
**Predecessors:** [`2026-05-10-voice-smooth-jitter-buffer-design.md`](2026-05-10-voice-smooth-jitter-buffer-design.md). Removing the Rust-side jitter pump regressed the per-peer level meter's auto-decay (the pump used to push silence-padded frames into the receive path, which decayed the EMA naturally). The fix that shipped with that PR added a JS-side decay timer — a targeted patch, not the right home for this logic.
**Successors:** None planned. After this lands, the level/loudness contract is owned by `sunset-voice`; future native hosts (TUI, Minecraft mod) get peer/self meters by implementing one trait.

## Goal

`sunset-voice` already owns the discrete signal "is this peer talking right now" (the `talking` boolean inside `VoicePeerState`, computed by the combiner from `frame_liveness`). Loudness is the same idea on a continuous axis — "*how loud* is this peer right now". Owning both signals in one place means:

- The runtime computes the smoothed level on whatever audio it actually has, instead of every host re-implementing the smoother.
- Native hosts (TUI, future Minecraft mod) get a level meter "for free" by implementing one trait.
- The level path becomes Rust-testable. The JS smoother is currently uncovered by unit tests because the repo has no node-level JS test runner.
- "Peer fell silent" has one source of truth in the same crate, instead of one boolean in Rust + one EMA in JS that can disagree under flake.

## Non-goals

- **UI-side visualization choices.** Bar colors, log-vs-linear axis, peak-hold behavior, gain calibration to "make speech reach 1.0" — these are *host* decisions and stay in the host. `sunset-voice` emits raw smoothed RMS in `0..1` (linear amplitude); the host decides how to render.
- **dBFS / loudness units.** The wire signal is linear amplitude. Hosts that want dB can apply `20 * log10(level)` themselves; that conversion is one line.
- **Adaptive calibration.** No auto-gain on the meter itself. The output is the literal smoothed RMS of the PCM that crossed the boundary.
- **Peak / VU / momentary-vs-integrated loudness.** A single smoothed RMS value per peer + one for self covers every visualization the project needs today. A future spec can extend `LevelSink` with peak/VU methods if a real consumer needs them.
- **Per-channel levels.** PCM is stereo on the playback path but we emit a single scalar (RMS over both channels). UIs that need per-channel can compute it themselves from the FrameSink PCM.
- **Backwards-compatibility shim** for the deleted JS-side decay. Deleted code, not deprecated.

## Architecture

```
┌──────────────────────────────────────────────────────────────┐
│ sunset-voice                                                  │
│                                                                │
│  send_pcm(pcm)                                                 │
│    └─ rms = compute_rms(pcm)                                   │
│       update self_level.ema  ──────────────┐                   │
│       stamp self_level.last_input_at       │                   │
│       (mute gate is downstream of this)    │                   │
│                                            │                   │
│  subscribe::Frame                          │                   │
│    └─ decode + denoise → pcm               │                   │
│       rms = compute_rms(pcm)               │                   │
│       update peer_levels[peer].ema  ───────┤ shared            │
│       stamp peer_levels[peer].last_input_at│ runtime state     │
│       FrameSink.deliver(peer, seq, pcm)    │                   │
│                                            ▼                   │
│  level_emitter task (every 50 ms):                             │
│    - for each peer:                                            │
│        if now - last_input_at > 30 ms:                         │
│            ema *= (1 - LEVEL_DECAY_ALPHA)                      │
│        snap to 0 below epsilon                                 │
│        level_sink.emit_peer(peer, ema)                         │
│    - same for self_level → level_sink.emit_self                │
└──────────────────────────────────────────────────────────────┘
                              │
                              ▼ host-supplied LevelSink impl
                  (WebLevelSink for browser, native TUI sink later)
```

Three sites feed the same EMA state:

1. **Receive-side RMS** (`subscribe.rs`): after decode + denoise, compute RMS of the 1920-sample stereo PCM, fold into the peer's EMA. This is what the user is *hearing*, post-Opus-quantization and post-denoise, so the meter matches the speaker output.
2. **Send-side RMS** (`runtime::send_pcm`): compute RMS of the captured PCM before the mute check. Muting silences the outgoing audio; it does not silence the local user's own meter (they expect to see their own speech move the bar regardless of mute state — this matches today's behavior and the inline comment in `voice.ffi.mjs::updateSelfLevel`).
3. **Decay tick** (`level_emitter` task): on every 50 ms tick, if no input arrived in the last 30 ms, multiply the EMA by `(1 - LEVEL_DECAY_ALPHA)`. This is the only mechanism that drives the level toward zero when audio stops.

## Components

### `LevelSink` trait (`crates/sunset-voice/src/runtime/traits.rs`)

```rust
/// Sink for continuous per-peer and self loudness updates. Fires at
/// `LEVEL_EMIT_INTERVAL` cadence regardless of whether any new audio
/// arrived this tick — the host can rely on the rate for animation
/// smoothing. Levels are smoothed RMS in `0..1` (linear amplitude).
/// UIs that want a normalized-for-speech scale apply their own gain
/// + clamp downstream.
pub trait LevelSink {
    fn emit_peer(&self, peer: &PeerId, level: f32);
    fn emit_self(&self, level: f32);
}
```

`?Send` is the workspace default; `LevelSink` follows.

### `RuntimeInner` additions (`runtime/state.rs`)

```rust
pub(crate) struct PeerLevel {
    pub ema: f32,
    pub last_input_at: web_time::Instant,
}

pub(crate) struct SelfLevel {
    pub ema: f32,
    pub last_input_at: Option<web_time::Instant>,
}

// In RuntimeInner:
pub level_sink: Rc<dyn LevelSink>,
pub peer_levels: RefCell<HashMap<PeerId, PeerLevel>>,
pub self_level: RefCell<SelfLevel>,
```

`SelfLevel::last_input_at` is `Option` because before the first `send_pcm` we have no captured frame to decay from — emit zero with no decay arithmetic.

### `level_emitter` task (`runtime/level_emitter.rs`)

```rust
pub(crate) fn spawn(weak: Weak<RuntimeInner>) -> LocalBoxFuture<'static, ()> {
    async move {
        loop {
            sleep(LEVEL_EMIT_INTERVAL).await;
            let Some(inner) = weak.upgrade() else { return; };
            let now = web_time::Instant::now();

            let mut to_emit_peer: Vec<(PeerId, f32)> = Vec::new();
            {
                let mut levels = inner.peer_levels.borrow_mut();
                for (peer, lvl) in levels.iter_mut() {
                    if now.duration_since(lvl.last_input_at) > LEVEL_DECAY_AFTER {
                        lvl.ema *= 1.0 - LEVEL_DECAY_ALPHA;
                    }
                    if lvl.ema < LEVEL_EPSILON { lvl.ema = 0.0; }
                    to_emit_peer.push((peer.clone(), lvl.ema));
                }
            }
            let self_emit = {
                let mut s = inner.self_level.borrow_mut();
                if let Some(t) = s.last_input_at {
                    if now.duration_since(t) > LEVEL_DECAY_AFTER {
                        s.ema *= 1.0 - LEVEL_DECAY_ALPHA;
                    }
                    if s.ema < LEVEL_EPSILON { s.ema = 0.0; }
                }
                s.ema
            };

            let sink = inner.level_sink.clone();
            drop(inner);
            for (peer, level) in to_emit_peer {
                sink.emit_peer(&peer, level);
            }
            sink.emit_self(self_emit);
        }
    }.boxed_local()
}
```

Mirror of `heartbeat::spawn` / `combiner::spawn`. Same `sleep` cfg-gate for WASM (via `wasmtimer`) as the rest of the runtime.

### RMS helper (`runtime/level.rs` or inline)

```rust
pub(crate) fn compute_rms(pcm: &[f32]) -> f32 {
    if pcm.is_empty() { return 0.0; }
    let sum: f32 = pcm.iter().map(|s| s * s).sum();
    (sum / pcm.len() as f32).sqrt()
}

pub(crate) fn fold_ema(prev: f32, new: f32) -> f32 {
    LEVEL_EMA_ALPHA * new + (1.0 - LEVEL_EMA_ALPHA) * prev
}
```

### Subscribe + send_pcm integration

`subscribe.rs` after denoise, before `frame_sink.deliver`:

```rust
let rms = compute_rms(&pcm);
let mut levels = inner.peer_levels.borrow_mut();
let entry = levels.entry(peer.clone()).or_insert(PeerLevel {
    ema: 0.0, last_input_at: web_time::Instant::now(),
});
entry.ema = fold_ema(entry.ema, rms);
entry.last_input_at = web_time::Instant::now();
```

`send_pcm` at the very top of the function (before mute, before format check):

```rust
let rms = compute_rms(pcm);
let mut s = self.inner.self_level.borrow_mut();
s.ema = fold_ema(s.ema, rms);
s.last_input_at = Some(web_time::Instant::now());
```

### Constants (`runtime/mod.rs`)

```rust
const LEVEL_EMA_ALPHA: f32 = 0.35;
const LEVEL_DECAY_ALPHA: f32 = 0.35;
const LEVEL_DECAY_AFTER: Duration = Duration::from_millis(30);
const LEVEL_EMIT_INTERVAL: Duration = Duration::from_millis(50);
const LEVEL_EPSILON: f32 = 0.001;
```

Values match today's JS implementation so the user-visible behavior doesn't shift.

### Peer cleanup

`auto_connect.rs` on `LivenessState::Stale`: in addition to `last_delivered_seq.remove`, also `peer_levels.remove(&ev.peer)`. The very next emitter tick won't emit for the gone peer; the host already removed its UI row via `frame_sink.drop_peer`.

There's a 0–50 ms window between "Rust removes the peer from `peer_levels`" and "the JS UI last received a non-zero level for that peer" — within which the Gleam row could still render a stale meter before the row itself is removed. The JS bridge handles this by, on `dropPeer`, immediately invoking the peer-level handler with `0.0` so the UI flushes to zero. This is the same pre-existing pattern: today's `voice.ffi.mjs::flushPeerLevelToZero` does exactly that.

### `VoiceRuntime::new` signature

Adds one parameter:

```rust
pub fn new(
    bus: Rc<dyn DynBus>,
    room: Rc<Room>,
    identity: Identity,
    dialer: Rc<dyn Dialer>,
    frame_sink: Rc<dyn FrameSink>,
    peer_state_sink: Rc<dyn PeerStateSink>,
    level_sink: Rc<dyn LevelSink>,  // ← new, required
) -> (Self, VoiceTasks);
```

`VoiceTasks` gains `level_emitter: LocalBoxFuture<'static, ()>`. Hosts must spawn it like they spawn the others. (No-op `LevelSink` impls are easy for hosts that don't want a meter — `impl LevelSink for () { ... }` — but the trait stays mandatory for shape consistency with `peer_state_sink`.)

### JS bridge (`crates/sunset-web-wasm/src/voice/`)

- New `level_sink.rs` with `WebLevelSink { on_peer_level: ..., on_self_level: ... }`, same pattern as `WebFrameSink`.
- `voice/mod.rs::voice_start` gains two `Function` params: `on_peer_level` and `on_self_level`. Threaded into a new `WebLevelSink`. The task list gains `tasks.level_emitter`.
- `Client::voice_start` (the wasm-bindgen entry) adds the two callbacks to its signature.

### Gleam FFI (`web/src/sunset_web/voice.ffi.mjs`)

Replaced:

- Delete: `peerLevelEma`, `peerLastFrameMs`, `peerLevelLastDispatchMs`, `selfLevelEma`, `selfLevelLastDispatchMs`, `levelDecayTimer`, `tickPeerLevelDecay`, `startLevelDecayTimer`, `stopLevelDecayTimer`, `updatePeerLevel`, `updateSelfLevel`, `flushPeerLevelToZero`, `flushSelfLevelToZero`, `computeRms`, all `LEVEL_*` constants except `LEVEL_RMS_GAIN`.
- Add: `wasmVoiceStart` passes two new callbacks. The peer-level callback receives `(peer_id: Uint8Array, level: f32)`, applies `LEVEL_RMS_GAIN * level`, clamps to `0..1`, dispatches to `window.__voicePeerLevelHandler`. Same for self via `window.__voiceSelfLevelHandler`.
- The `__voiceFfi.getPeerLevel` test handle is preserved by keeping a local cache of the last *normalized* value the JS dispatched (the e2e tests assert on the normalized level, not raw RMS).

`deliverFrame` no longer touches level state; the only remaining frame-side bookkeeping is the worklet `postMessage`.

## Data flow

| Event | What happens |
|---|---|
| Capture worklet hands PCM to `voice_input` | Crosses FFI → `runtime.send_pcm(pcm)` → RMS computed, `self_level.ema` updated, `last_input_at` stamped. Then existing mute/encode/publish path runs. |
| WebRTC datachannel delivers a voice frame | `subscribe.rs` decrypts/decodes/denoises, computes RMS, updates `peer_levels[peer]`, calls `FrameSink.deliver` as today. |
| `level_emitter` tick (every 50 ms) | Iterates `peer_levels` (decay if stale, emit), then `self_level` (same). One emission per peer per tick. |
| Peer goes Stale (`auto_connect`) | `peer_levels.remove(&peer)` alongside the existing cleanup. |
| `VoiceRuntime` dropped | All tasks observe the `Weak` upgrade failure and exit, including `level_emitter`. |

## Failure modes

| What can go wrong | What happens | Is that OK? |
|---|---|---|
| LevelSink impl panics | Panic bubbles into the worklet task → `wasm_bindgen_futures::spawn_local` panic → page-level pageerror. | Same blast radius as PeerStateSink panicking today. Treat impls as trusted hosts. |
| `Instant::now()` skew (clock jump) | `now - last_input_at` becomes negative → `Duration::from_*` saturates / the comparison `> LEVEL_DECAY_AFTER` returns false → no decay on that tick. Self-corrects on the next tick. | Yes — at most one tick of meter staleness. |
| EMA → NaN (bad input) | `pcm` containing NaN gets folded into the EMA. Subsequent emissions are NaN. | Add `if rms.is_finite()` guard in `fold_ema`. Decoded PCM is guaranteed finite by the existing test invariants; the guard is defense-in-depth. |
| `peer_levels` map grows unboundedly | Bounded by the number of peers we've ever heard from. Cleanup on Stale (above) keeps it bounded by the active room. | Yes. |
| Tick runs late (WASM timer drift) | Emissions get bunched. Visual smoothness degrades momentarily; EMA is still correct on next tick. | Yes — same flavour of timer drift the playback worklet was designed around, but here the consumer is a UI bar, not the audio device. UI bars tolerate jitter. |
| Host doesn't spawn `level_emitter` | No emissions ever fire. EMA state still updates internally (harmless). | Yes — same as not spawning `combiner`. |

## Testing

### Rust unit tests (`runtime_integration.rs` or `runtime_levels.rs`)

1. **Peer level rises on frame arrival.** Inject one synthetic decoded frame at amplitude 0.2 (RMS ≈ 0.2). Spawn `level_emitter`. Wait one tick. Assert `RecordingLevelSink` saw `emit_peer(alice, > 0.05)` (the EMA after one fold of 0.2 with α=0.35 is 0.07; > 0.05 with slack).
2. **Peer level decays to ~0 within ~300 ms.** Inject one frame at amplitude 0.5 (RMS ≈ 0.5). Wait one tick to register. Stop injecting. Advance time; assert the level emitted by tick ~6 is below `LEVEL_EPSILON`.
3. **Self level rises on `send_pcm`.** Call `send_pcm` with synthetic PCM at amplitude 0.3. Wait one tick. Assert `emit_self` saw a value > 0.05.
4. **Self level decays after capture stops.** Same shape as peer decay.
5. **Muted state does not gate self level.** Set muted, call `send_pcm`. Assert `emit_self` still emits a non-zero level.
6. **Deafened decays peer level naturally.** The existing deafened path in `subscribe.rs` continues to skip decode entirely. With no decoded PCM, no frames feed the per-peer EMA, so the `level_emitter` task decays it to zero on the normal schedule. This is the correct UX — "I'm deafened" means "I can't hear them", so a flat zero meter matches what's audible. No special-case in `level_emitter`. No new test fixture beyond the existing decay test.
7. **NaN input doesn't poison the EMA.** Feed a single NaN sample. Assert subsequent emissions remain finite.
8. **`level_emitter` exits on runtime drop.** Extend `dropping_runtime_terminates_all_tasks` to include `level_emitter`.
9. **Peer Stale clears the level entry.** Mirror the existing `auto_connect` Stale test; observe that `peer_levels` no longer contains the gone peer after the Stale event.

### E2E (`web/e2e/voice_channel_roster.spec.js`)

Already covers "Alice talks → Bob's meter rises → Alice stops → Bob's meter falls within 3 s." Spec assertion is on the JS handler's reported value, which now comes from the Rust LevelSink → WebLevelSink → callback path. Test should continue to pass without modification, because (a) the smoothing alpha is unchanged, (b) the decay shape is unchanged (just driven from Rust instead of JS), and (c) the LEVEL_RMS_GAIN is applied at the same point in the dispatch path.

## Rollout

Single PR, no flag. The trait change is sourcecompatible because nothing outside the workspace consumes `VoiceRuntime::new`. Hosts in this repo:

- `sunset-web-wasm` updated in the same PR.
- Tests updated in the same PR.

The web Gleam UI is untouched — the FFI hides the new callbacks behind the existing `__voicePeerLevelHandler` / `__voiceSelfLevelHandler` window functions that Gleam already wires.
