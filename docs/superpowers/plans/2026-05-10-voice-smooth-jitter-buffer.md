# Voice: smooth jitter buffer — implementation plan

**Spec:** `docs/superpowers/specs/2026-05-10-voice-smooth-jitter-buffer-design.md`
**Branch:** `voice-jitter-smooth`

The plan is sequenced so each commit compiles and tests pass. Tests are written before the production change they verify (TDD per CLAUDE.md).

## Step 1 — Update `FrameSink` trait + propagate `seq` through Rust

Goal: every `FrameSink::deliver` call receives a 32-bit sequence number derived from `VoicePacket::Frame::seq`. The Rust side compiles after this step; no tests are broken yet because we keep the wall-clock pump in place and route the seq through it.

1.1 **Edit `crates/sunset-voice/src/runtime/traits.rs`:** change `deliver(&self, peer, pcm)` → `deliver(&self, peer, seq, pcm)`. The doc comment is updated to describe `seq` as the truncated low 32 bits of `VoicePacket::Frame::seq`, used by hosts for sequence-indexed buffering and gap detection.

1.2 **Edit `crates/sunset-voice/src/runtime/subscribe.rs`:** when pushing to the jitter buffer, also push the seq alongside the pcm. The jitter buffer type changes from `VecDeque<Vec<f32>>` to `VecDeque<(u32, Vec<f32>)>`.

1.3 **Edit `crates/sunset-voice/src/runtime/state.rs`:** jitter map value type updated to match. `LastDelivered` is kept temporarily — it will be deleted in Step 3.

1.4 **Edit `crates/sunset-voice/src/runtime/jitter.rs`:** pop yields `(seq, pcm)`. Call site delivers `frame_sink.deliver(peer, seq, &pcm)`. The repeat-last branch still emits the same seq as the original last delivery (since the worklet won't actually be using these clones after Step 4 — but for now we keep semantics tight).

1.5 **Edit `crates/sunset-voice/src/runtime/state.rs::LastDelivered`:** add `seq: u32`. Update pump to fill it.

1.6 **Edit `crates/sunset-voice/tests/runtime_integration.rs`:** every `impl FrameSink for ...` updated to take `seq: u32`. `RecordingFrameSink::deliver` stores `(peer, seq, pcm)`. Existing assertions about frame contents become `pcm`-only checks against the new tuple shape. Specifically: `subscribe_decrypts_frame_and_pushes_to_jitter` still uses `test_jitter_len`; that test continues to work.

1.7 **Update `crates/sunset-web-wasm/src/voice/frame_sink.rs`:** `deliver(peer, seq, pcm)` includes `seq` in the JS callback. The on_pcm callback signature is now `(peer_id: Uint8Array, seq: u32, pcm: Float32Array)`.

1.8 **Update `crates/sunset-web-wasm/src/voice/test_hooks.rs::RecordingFrameSink::deliver`:** takes `seq`, stores it in `RecordedFrame`. Optional: add a `seq: u32` field to `RecordedFrame`. (We add it — gives us a debugging affordance even if no existing test uses it.)

1.9 **Update the JS-side `voice_install_frame_recorder` / `voice_recorded_frames` plumbing in `crates/sunset-web-wasm/src/client.rs`:** the JS return value includes `seq` per frame.

1.10 **Update the JS bridge** that wires `on_pcm` to the playback worklet: pass `seq` along with `pcm` in `postMessage`. Worklet for now ignores `seq` (one-line change there).

After Step 1: `cargo test --workspace --all-features` + `cargo clippy --workspace --all-features --all-targets -- -D warnings` + `cargo fmt --all --check` must pass.

### Step 1 review checkpoint

Verify:
- All `FrameSink` impls in test files updated
- The recorder ring buffer's checksum logic is unchanged
- No clippy warnings (esp. `clippy::too_many_arguments` from the new signature)

## Step 2 — New playback worklet logic (still parallel to old path)

Goal: rewrite `voice-playback-worklet.js` to use the new state machine (Warmup / Playing / Underrun with cosine fades), but the worklet still receives frames from the existing path and the new logic is exercised by the existing e2e tests.

2.1 **Replace `web/audio/voice-playback-worklet.js`:**

- `Map<u32, Float32Array>` keyed by seq (frames are pushed in via `port.onmessage` as `{seq, pcm}`).
- State enum `Warmup | Playing | Underrun`.
- Constants: `TARGET_PLAYOUT_DEPTH = 3`, `MAX_DEPTH = 10`, `FADE_SAMPLES = 240`.
- `process(_, outputs)`: drain `outputs[0][0..n]` and `outputs[0][1..n]` interleaved per channel.
- On insert: drop oldest if over `MAX_DEPTH`; transition Underrun → Playing if depth ≥ TARGET.
- Cosine fade is `0.5 - 0.5 * Math.cos(Math.PI * progress)` from 0..1 (progress = sample_idx / FADE_SAMPLES); apply as a multiplier.
- The fade-out branch is entered the moment `process()` discovers `this.head` is `null` and `queue.size == 0`. The faded tail is *the last frame's samples replayed with fade*: store the last popped frame and the within-frame offset, so the fade-out can read forward into "the rest of the would-be frame" and ramp those samples down. This is the gentle equivalent of "repeat last frame" — but only for the fade window, not the whole frame.

  Detail: when in Playing and we run out of head, we don't have any next data to fade against, so we synthesize the fade-out by:
  - keeping the last fully-delivered frame in `lastFrame` (Float32Array, 1920 long)
  - keeping `lastFrameOffset` = how many sample pairs of `lastFrame` we'd already played
  - on entering Underrun, we play `lastFrame[lastFrameOffset..lastFrameOffset+FADE_SAMPLES]` multiplied by a cosine ramp `1 → 0`. If `lastFrameOffset + FADE_SAMPLES > FRAME_SAMPLES_PER_CHANNEL`, we wrap or clip to silence — clip is fine because we're fading to zero anyway.
  - after the fade-out completes, output zeros until the next fade-in trigger.

- Fade-in: when transitioning Underrun → Playing, the first `FADE_SAMPLES` pairs are the new head frame's first `FADE_SAMPLES` pairs multiplied by `0 → 1` cosine ramp.

2.2 **Add a test hook to record worklet output** (test-only):

- New worklet file `web/audio/voice-output-recorder-worklet.js` that captures its input into a ring buffer (per-channel ~5 seconds) and posts the buffer to the main thread on `port.postMessage('snapshot')`.
- New WASM client methods `voice_install_output_recorder() -> Result<(), JsError>` and `voice_output_recorded_samples() -> Result<JsValue, JsError>` (gated behind `feature = "test-hooks"`).
- The JS bridge wires the recorder worklet between the playback worklet and the destination when installed.

This hook is the only piece of new test infrastructure. It's small (~80 lines JS + ~30 lines Rust) and is gated behind test-hooks so it doesn't ship in production builds.

After Step 2: existing e2e tests pass. The new path is exercised by them because the Rust pump still delivers frames; the worklet just buffers + fades them.

## Step 3 — Delete the Rust-side pump; subscribe delivers directly

Goal: the Rust pump is removed; subscribe.rs calls `frame_sink.deliver` directly. The deafened path moves from "pump drain" to "skip decode".

3.1 **Edit `crates/sunset-voice/src/runtime/subscribe.rs`:** on `Frame`, if `*inner.deafened.borrow()` is true, `continue`. Else after decode + denoise, call `inner.frame_sink.borrow().deliver(&peer, seq as u32, &pcm)` directly. Remove the `let mut jitter = inner.jitter.borrow_mut(); ... q.push_back(pcm);` block.

3.2 **Edit `crates/sunset-voice/src/runtime/mod.rs`:**
- Remove `JITTER_MAX_DEPTH`, `JITTER_PUMP_INTERVAL`.
- Remove `tasks.jitter_pump` field from `VoiceTasks`.
- Remove `tasks.jitter_pump: jitter::spawn(...)` from `VoiceTasks::new`.
- Remove the `mod jitter;` declaration.
- Remove the `#[cfg(feature = "test-hooks")] fn test_jitter_len` and `test_push_frame` and `jitter_depths` and `observed_voice_peers` (the last one needs to be re-implemented since the jitter map is gone — re-implement against `last_emitted`).

3.3 **Edit `crates/sunset-voice/src/runtime/state.rs`:** remove `jitter`, `last_delivered`, and `LastDelivered`.

3.4 **Delete `crates/sunset-voice/src/runtime/jitter.rs`** (file no longer referenced).

3.5 **Edit `crates/sunset-voice/tests/runtime_integration.rs`:**
- `subscribe_decrypts_frame_and_pushes_to_jitter` → rename to `subscribe_decodes_frame_and_delivers_to_sink`. Use a `RecordingFrameSink` (already in this file) instead of `test_jitter_len`. The assertion becomes: `delivered` vector contains exactly one entry with `seq=1` and the expected PCM length.
- `jitter_pump_delivers_at_20ms_cadence_and_pads_silence` → **delete**. This tests behavior we're explicitly removing; the new contract is "deliver immediately", which is verified by the rewritten test above.
- `dropping_runtime_terminates_all_tasks` → remove `tasks.jitter_pump` from the spawn list and from `task_names`.
- `set_denoise_toggle_attenuates_inbound_noise` → remove `spawn_local(tasks.jitter_pump)`. The RmsSink will now see frames as soon as subscribe decodes them. The 25-frames wait still works the same way.
- Add a new test `deafened_skips_decode_and_delivery`: set deafened, inject one frame, wait, assert FrameSink received zero frames.

3.6 **Edit `crates/sunset-voice/tests/runtime_skeleton.rs` and `runtime_traits.rs`** if either references `tasks.jitter_pump` or `test_jitter_len`. (Will check during execution.)

After Step 3: `cargo test --workspace --all-features` passes, including the new deafened test. Clippy clean.

### Step 3 review checkpoint

Verify with `git grep`:
- No remaining references to `jitter::`, `JITTER_PUMP`, `JITTER_MAX_DEPTH`, `last_delivered`, `LastDelivered`.
- The remaining test-hook methods (`set_frame_sink`, `snapshot_states`, `auto_connect_peers`, `test_liveness`) still compile.
- `cargo doc` doesn't error on broken intra-doc links to removed items.

## Step 4 — New e2e: smoothness under simulated packet loss

Goal: an automated check that the click-pattern fix actually works at the end-to-end audio level.

4.1 **Add a WASM test hook `voice_drop_every_nth(n: u32)`** that, when set, drops every Nth outbound frame in `send_pcm` *before* it hits the encoder/bus. Gated behind `feature = "test-hooks"`.

4.2 **Add `web/audio/voice-output-recorder-worklet.js`** (from Step 2.2).

4.3 **Add WASM bindings `voice_install_output_recorder()` / `voice_output_recorded_samples()`** (from Step 2.2).

4.4 **Add `web/e2e/voice_smoothness.spec.js`:**

- Open Alice + Bob through the real Gleam UI (mirror existing voice_two_way.spec.js pattern).
- Both join voice.
- Bob installs the output recorder.
- Alice calls `voice_drop_every_nth(7)` (drops ~14% of frames — realistic moderate loss).
- Alice injects ~100 frames of synthetic continuous tone via `voice_inject_pcm`.
- Wait for Bob's output recorder to have recorded ~1.5 seconds of samples.
- Assert: `max(abs(samples[i+1] - samples[i]))` across the recorded window is below a click threshold. The threshold is tuned empirically — for a 0.5-amplitude 440 Hz sine input, the natural max sample-to-sample delta is `~0.5 * 2π * 440 / 48000 ≈ 0.029`. A click typically produces deltas of 0.1+ at frame boundaries. We assert `< 0.06` (2× natural max, well under click range).
- Also assert: at least 80% of recorded samples are non-zero (proves the buffer isn't just permanently in silent Warmup).

After Step 4: the new e2e passes locally and on CI.

## Step 5 — Final pass + push

5.1 Run the workspace gate:
- `nix develop --command cargo fmt --all --check`
- `nix develop --command cargo clippy --workspace --all-features --all-targets -- -D warnings`
- `nix develop --command cargo test --workspace --all-features`
- `nix develop --command bash scripts/check-no-clippy-allow.sh`

5.2 Run the affected e2e suites locally with the bundled playwright config:
- `voice_two_way.spec.js`, `voice_quality.spec.js`, `voice_three_way.spec.js`, `voice_denoise.spec.js`, `voice_mute_deafen.spec.js`, `voice_smoothness.spec.js` (new).

5.3 Commit in logical units:
- Commit 1: spec + plan
- Commit 2: trait change + recorder hook (Step 1)
- Commit 3: new playback worklet logic + output recorder hook (Step 2)
- Commit 4: drop the Rust pump + tests (Step 3)
- Commit 5: smoothness e2e (Step 4)

Or fewer larger commits if subagent-driven-development surfaces a cleaner grouping.

5.4 Push, open PR, watch CI per `working-autonomously-on-prs`.

## What can go wrong

| Risk | Mitigation |
|---|---|
| The fade-out reads past `lastFrame`'s tail (the underrun starts mid-frame and we already played most of it). | Clip the fade-out to `FRAME_SAMPLES_PER_CHANNEL - lastFrameOffset` samples; if `<FADE_SAMPLES` remain, do a shorter fade or fade to zero instantly (the tail of `lastFrame` was already near-zero in the natural case for voice; for a sustained tone it'll still be a tiny click but smaller than today). Acceptable. |
| Worklet's `seq` arithmetic wraps after ~24 hours of continuous voice (u32 at 50 fps = ~24 days, actually). | Use modular comparison only for "is this seq > last_played_seq" via `(a - b) > 0` with signed subtraction; we expect zero wraps in practice. |
| The output-recorder worklet adds processing latency. | It's a passthrough — `process(inputs, outputs) { copy in→out; record; return true; }` — so no added latency beyond a single AudioWorklet hop. Existing tests should still pass. |
| The 60 ms playout buffer is perceptible as latency. | Spec already calls out we can dial it down to 2 frames (40 ms) if the user complains. Single constant change. |
| Clock drift between sender/receiver causes long-term drift in buffer fill. | `MAX_DEPTH` drops oldest. Long-term we'd want fractional resampling; deferred per spec non-goals. |
| WebRTC unordered datachannel reorders frames widely. | The seq-indexed map handles reorders within a window. Beyond `MAX_DEPTH` frames of reordering, we drop late-arrivals — same as today. |
| The new smoothness e2e is flaky on CI due to WebRTC handshake variance. | Use the existing `voice_two_way` infrastructure; we know its handshake budget. Tune the wait timeout if needed. Don't add `waitForTimeout` for racy state. |
