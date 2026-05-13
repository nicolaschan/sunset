# Voice: smooth jitter buffer (worklet-side) design

**Date:** 2026-05-10
**Scope:** Eliminate the choppy / stuttering / clicking sound from the voice playback path. Keep added latency low (~60 ms target buffer fill on top of existing pipeline latency).
**Predecessors:** Voice pipeline (C2a/b/c), Opus codec swap, receiver-side RNNoise — all merged.
**Successors:** None planned. This is a quality fix, not a new feature.

## Goal

The voice path currently sounds choppy with audible clicks during network jitter or load-induced timer jitter. The clicks happen because of three compounding root causes:

1. **The receive-side jitter buffer is paced by a wall-clock `tokio::time::sleep(20ms)` Rust task** that hands one frame per tick to the playback worklet. That timer drifts relative to the device's audio crystal and is throttled when the tab is backgrounded. When the JS event loop is busy, the timer fires late; the worklet's small internal queue empties; the worklet outputs zeros until the next pump tick. Cyclic underflow → cyclic gaps → "stutter".

2. **Underrun "PLC" is brutal.** On the first missed pump, the runtime repeats the previous frame in full. The repeat's tail samples join the same frame's head samples at a phase boundary — that's a step discontinuity. Then if the next tick is also empty, the runtime emits raw zeros, another step discontinuity. Each step is a click. Insanity holds the *last sample* (DC); that's bad too, but it's less audible than a phase discontinuity inside a sine wave.

3. **No target playout depth.** The runtime delivers frames the instant the pump fires, never building headroom. Any network jitter punches straight through into audible underrun.

The fix is to delete the wall-clock pump and let the audio device's clock pace consumption end-to-end, the way Insanity does with `cpal`'s callback. The browser equivalent is the `AudioWorkletProcessor::process` callback, which fires at exactly `128 samples / sampleRate` cadence regardless of JS event-loop weather. We move the per-peer jitter buffer into that worklet and add a small playout depth + cosine-faded PLC on top.

## Non-goals

- **FEC / inband packet recovery.** Opus supports inband FEC; layering it in would require sender-side changes and is out of scope.
- **Adaptive playout depth.** The depth is constant. A real Discord-style adaptive jitter buffer measures jitter and grows under stress; deferred.
- **Sender-side pacing.** We don't change anything about the encode/publish path.
- **Native (TUI) voice playback.** Native voice doesn't exist yet (Plan deferred). The Rust trait stays compatible so the future native host gets a (potentially different) jitter buffer at the cpal layer.
- **Echo cancellation / AGC / DTX.** Browser handles AEC; AGC and DTX are codec-level, untouched.
- **Visualizer changes.** The waveform meter reads from a different path (capture-side); unaffected.

## Architecture (target)

```
sender side (unchanged)
   ↓
WebRTC datachannel
   ↓
sunset-voice::runtime::subscribe (Rust)
   - decrypt
   - decode Opus → 1920-sample stereo PCM
   - denoise per-peer (existing)
   - call frame_sink.deliver(peer, seq, pcm)        ◄── direct, no wall-clock pump
   ↓
WebFrameSink (Rust → JS)
   - postMessage to per-peer playback worklet:
     {pcm: Float32Array, seq: u32}
   ↓
voice-playback-worklet (JS)
   - sequence-indexed slot buffer (ring)
   - target playout depth (warmup before first sample)
   - cosine fade-out on underrun, cosine fade-in on recovery
   - overflow: drop oldest
   - process() pulls samples at the audio-device clock
   ↓
audio output
```

Two structural changes:

1. **Rust side**: delete `runtime/jitter.rs`, delete `RuntimeInner::jitter` and `last_delivered`, delete `JITTER_MAX_DEPTH`, `JITTER_PUMP_INTERVAL`, delete `tasks.jitter_pump`. The subscribe path delivers frames to the sink as soon as they're decoded. The `FrameSink::deliver` signature gains a `seq: u32` parameter (current sequence numbers in `VoicePacket::Frame` are `u64`; we truncate to `u32` at the FFI boundary because the worklet only needs gap detection within a small window, and Float32Array postMessage already has cost — 32-bit seq fits in a single Uint32 transferable).

2. **JS side**: the playback worklet replaces its FIFO `queue` with the playout state machine described below. The bridge (`voice/frame_sink.rs` + `client.rs`) is updated to pass `seq` through.

### Worklet state machine

Three states: `Warmup`, `Playing`, `Underrun`.

```
              first frame arrives
 ┌──────────┐                       ┌────────────┐
 │ Warmup   │ ────────────────────▶ │ Playing    │
 │ (silent) │   depth ≥ TARGET      │            │
 └──────────┘                       └────────────┘
                                          │ ▲
                       queue empty mid-   │ │ queue refills
                       playback           │ │ ≥ TARGET
                                          ▼ │
                                    ┌────────────┐
                                    │ Underrun   │
                                    │ (faded     │
                                    │  silence)  │
                                    └────────────┘
```

- **Warmup**: output silence until the buffer has accumulated `TARGET_PLAYOUT_DEPTH` frames. Then transition to `Playing`. We could fade-in here too (cosine ramp from 0 → 1 over `FADE_SAMPLES`) so the very first audible sample isn't a step. Worth doing.
- **Playing**: pull samples from buffer at the audio clock. If the buffer empties mid-frame, transition to `Underrun` and immediately start a cosine fade-out (in the same `process()` call — write the faded samples directly).
- **Underrun**: output silence. When `queue.length >= TARGET_PLAYOUT_DEPTH`, fade-in over `FADE_SAMPLES` and transition to `Playing`. The fade-in's tail crossfades with the *new* frame's head; the fade-out's tail is also smoothed against silence. Both edges of the gap are cosine-tapered → no click.

### Buffer layout

A `Map<seq, Float32Array>` keyed by the 32-bit sequence number. We don't use a slot-mod-N ring like Insanity because we don't need O(1) random access by seq — frames arrive nearly in order, and we just need to (a) detect gaps to know when to fade out, and (b) keep total fill bounded.

Operations:

- **insert(seq, pcm)**: store in map. If `map.size > MAX_DEPTH`, drop the entry with the smallest seq. If `state == Underrun && map.size >= TARGET_PLAYOUT_DEPTH`, start fade-in.
- **pop_next()**: return the entry with the smallest seq, and remove it. Track `last_played_seq` so the worklet can spot a gap (`next_seq > last_played_seq + 1`) on pop.
- **fill()**: number of frames currently in the map.

Gaps: when `pop_next` discovers `next_seq > last_played_seq + 1`, the playback continues from `next_seq` *as if it were sequential*. We don't insert synthetic missing-frames; the listener perceives a tiny audible discontinuity only if the gap is wide (the seqs are dense enough relative to playout depth that the missing frame is almost always already abandoned). The cosine fade-out → fade-in cycle around the underrun already smooths this case.

### Tuning constants

| Constant | Value | Why |
|---|---|---|
| `TARGET_PLAYOUT_DEPTH` | 3 frames (60 ms) | Enough headroom to absorb typical Wi-Fi RTT variance (~15-30 ms) plus one OS scheduler hiccup. Trades ~60 ms of one-way latency for click-free playback. Discord-quality voice apps run with 30-100 ms playout buffers; 60 ms is a deliberately conservative starting point — easy to drop to 2 frames (40 ms) later if users complain about latency. |
| `MAX_DEPTH` | 10 frames (200 ms) | Defends against pathological clock-skew where the sender's audio clock is meaningfully faster than the receiver's. Beyond this, the listener is hearing audio from 200 ms ago — better to drop and resync. |
| `FADE_SAMPLES` | 240 (5 ms @ 48 kHz, per channel) | Cosine fade window. Long enough to be inaudible as a transient (a 5 ms fade is below the ear's transient threshold for music; well below for speech), short enough not to noticeably extend the gap. Insanity's hold-last-sample is functionally an infinite DC fade with zero ramp — that's why it pops less than full-frame repeat but still clicks at the edges. |

### Failure modes & responses

| What can go wrong | What happens | Why this is acceptable |
|---|---|---|
| Network drops one frame | seq gap → consumer plays adjacent frames consecutively. If buffer is at target depth, listener hears a tiny phase glitch (one frame missing in a sequential stream). For Opus this is barely audible because Opus's PLC during decode (if we enabled it) covers the gap — but we don't pass null to `decoder.decode()` for the missing frame, so the listener just gets a 20 ms sequence skip. | One missing frame = 20 ms gap. The fade is only active for *sustained* underrun (multi-frame), so the click pattern we set out to fix won't reappear. |
| Network burst → many frames at once | `insert` caps at `MAX_DEPTH` by dropping oldest. The listener catches up to live. | A 200 ms catch-up artifact is rare and recoverable; an unbounded growing buffer is unrecoverable. |
| Sender clock drifts faster than receiver | Buffer grows above target until it hits MAX_DEPTH, then drops oldest periodically. | Same as above. Long-term clock-skew adaptation is deferred (a more advanced jitter buffer would slow down playback fractionally to absorb it). |
| Receiver clock drifts faster than sender | Buffer drains until underrun; fade-out + silence; refill when sender catches up. | Mirror of above. Long-term drift is a future feature. |
| Worklet is destroyed while frames are in flight | Postmessage to a dead port is a no-op (browser handles it). No crash. | Existing behavior; unchanged. |

### Deafened path

`set_deafened(true)` in the runtime currently routes through the jitter pump, which silently drains. With the pump deleted, we need an alternative point to suppress audio. Two options:

- **A — suppress in subscribe**: when `deafened.borrow()` is true, skip the decode entirely. Saves CPU. But: `set_deafened` is interior-mutable, so a packet in flight in the middle of decode finishes anyway, and the next packet sees the new flag. Acceptable.
- **B — suppress in FrameSink**: pass the frame through to JS but JS gates output. More work, no advantage.

We pick **A**: the subscribe loop checks `inner.deafened.borrow()` and skips decode + delivery when true. Minor wrinkle: when un-deafened, the worklet may be in `Warmup` again because no frames arrived for a while — fine, that's the user-correct UX (un-deafen has a tiny warmup cost which matches what they'd hear if they were just joining the call).

## Components and changes

| File | Change |
|---|---|
| `crates/sunset-voice/src/runtime/traits.rs` | `FrameSink::deliver(peer, seq, pcm)` adds `seq: u32`. |
| `crates/sunset-voice/src/runtime/mod.rs` | Remove `JITTER_MAX_DEPTH`, `JITTER_PUMP_INTERVAL`. Remove `tasks.jitter_pump` from `VoiceTasks`. |
| `crates/sunset-voice/src/runtime/jitter.rs` | **Deleted.** |
| `crates/sunset-voice/src/runtime/state.rs` | Remove `jitter`, `last_delivered`, `LastDelivered` struct. |
| `crates/sunset-voice/src/runtime/subscribe.rs` | On `Frame`: if `deafened`, skip. Else decode → denoise → `frame_sink.deliver(peer, seq as u32, &pcm)`. No buffer push. |
| `crates/sunset-voice/tests/runtime_integration.rs` | Update `RecordingFrameSink::deliver` to take `seq`. Rewrite `subscribe_decrypts_frame_and_pushes_to_jitter` to assert frame appears in FrameSink (not jitter buffer). Delete `jitter_pump_delivers_at_20ms_cadence_and_pads_silence` (tests behavior being removed). Update `dropping_runtime_terminates_all_tasks` to not reference `tasks.jitter_pump`. Update `set_denoise_toggle_attenuates_inbound_noise` to not spawn jitter pump. |
| `crates/sunset-web-wasm/src/voice/frame_sink.rs` | `deliver(peer, seq, pcm)` posts `{seq, pcm}` to the JS callback (as two args or a small object). |
| `crates/sunset-web-wasm/src/voice/test_hooks.rs` | `RecordingFrameSink::deliver` records `seq` too (optional; default to recording without it for now). |
| `crates/sunset-web-wasm/src/voice/mod.rs` | Update `on_pcm` JS callback shape and pass-through. |
| `web/audio/voice-playback-worklet.js` | Replace FIFO queue with seq-indexed `Map<u32, Float32Array>` + state machine + cosine fades. |
| Wherever JS calls `voice_install_frame_recorder` posts to worklet | Pass `seq` along with `pcm` in the postMessage. |

## Testing

### Unit tests (Rust)

1. **Frame is delivered to FrameSink with correct seq.** Inject one frame from peer A with `seq = 42`; FrameSink receives `(peer_a, 42, pcm)`.
2. **Deafened skips delivery.** Set deafened, inject frame, verify FrameSink never called.
3. **Denoise toggle still works.** Existing `set_denoise_toggle_attenuates_inbound_noise` test, adapted to not spawn jitter_pump and to inspect frames as they arrive directly.

### Worklet unit tests (JS)

The repo has no node-level JS unit test runner (only Playwright + Gleam tests). Setting one up is out of scope for this PR. The worklet logic is therefore inline in the worklet file, and we validate it via:

- **A new e2e test** (`voice_smoothness.spec.js`) that drives two peers through the real Gleam UI, records the worklet's *output samples* (not the FrameSink input — that's a different point in the pipeline), and asserts the inter-sample discontinuity stays under a click-audibility threshold even with simulated packet loss.

  Recording worklet output requires a new test hook: a second `AudioWorkletProcessor` ("recording worklet") wired in series after `voice-playback`. The recording worklet captures its input and exposes it via `port.postMessage`. Gated behind `SUNSET_TEST` and `voice_install_output_recorder()` on the WASM client.

  This hook is the only piece of testability infrastructure added in this PR; everything else uses existing hooks.

### Playwright e2e (regression)

The existing `voice_two_way`, `voice_quality`, `voice_three_way`, `voice_denoise`, `voice_real_mic`, `voice_mute_deafen`, `voice_churn`, `voice_protocol`, `voice_channel_roster`, `voice_mic_permission` e2e tests must continue to pass unchanged. The contract change to `FrameSink` is internal to the runtime; the recorder hook used by these tests (`RecordingFrameSink`) is updated to accept the new `seq` parameter but its observable surface (`voice_recorded_frames` return shape) is unchanged.

### Manual listening

The user reported the problem from listening. They will validate the fix the same way. We rely on this as the final UX-level signal — the e2e click-detection asserts a sample-level property, but "does it sound smoother in the wild" is the actual product question.

## Latency budget

| Stage | Latency before | Latency after |
|---|---|---|
| Capture worklet → encode | ~20 ms (one frame) | unchanged |
| Network (WebRTC) | ~20-50 ms typical | unchanged |
| Decode + push to JS | ~1 ms | unchanged |
| **Receive-side jitter buffer** | ~0 ms (pump runs immediately) plus jitter from wall-clock pump | **~60 ms (target playout depth) + reduced variance** |
| Worklet → speaker | ~10 ms (audio engine buffer) | unchanged |
| **Total** | ~50-90 ms | ~110-150 ms |

Net change: **+60 ms one-way latency** in exchange for elimination of the click pattern. For voice chat in a friend-group context, 150 ms one-way is below the threshold where users notice conversational latency (typically ~250 ms). The user's prompt explicitly tolerated "not adding much latency" and 60 ms qualifies; if it turns out to be too much we can dial `TARGET_PLAYOUT_DEPTH` down to 2 (40 ms) without changing the design.

## Rollout

Single PR. No feature flag — the change is small, well-isolated, and the fix is to remove choppiness that is already audible. If it regresses we revert; we don't ship two playback paths.
