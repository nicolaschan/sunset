# sunset-voice Audio Pipeline (C2a) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Get the audio path working end-to-end inside one browser tab — real microphone in, real speakers out, with a complete Opus encode + Opus decode in between, no networking. After C2a we know the codec + audio bridge work before C2b layers encryption + Bus on top.

**Architecture:** A new `sunset-voice` crate exposes a Rust `VoiceEncoder` / `VoiceDecoder` over the `opus` crate (libopus). `sunset-web-wasm` adds a `voice` module with three wasm-bindgen methods on `Client` (`voice_start`, `voice_stop`, `voice_input`) and an in-Rust loopback queue that connects encoder output to decoder input. Two AudioWorkletProcessor JS files handle the 128-sample-quantum ↔ 960-sample-frame buffering on the audio thread. A throwaway HTML demo page wires `getUserMedia` through the whole pipeline; speaking into the mic and hearing yourself is the C2a acceptance test.

**Tech Stack:** Rust + `opus` crate (libopus C library) + wasm-bindgen + `js-sys`/`web-sys` + AudioWorklet API + `getUserMedia` + Nix flake (libopus added to buildInputs).

**Spec:** `docs/superpowers/specs/2026-04-28-sunset-voice-pipeline-design.md`

---

## File Structure

| File | Responsibility |
|---|---|
| `crates/sunset-voice/Cargo.toml` (new) | Crate manifest; `opus` + `thiserror` deps; `[lints] workspace = true` |
| `crates/sunset-voice/src/lib.rs` (new) | `VoiceEncoder`, `VoiceDecoder`, `Error`, constants, co-located unit tests |
| `Cargo.toml` (modify) | Add `crates/sunset-voice` to `workspace.members` |
| `flake.nix` (modify) | Add `pkgs.libopus` to dev-shell buildInputs + sunset-web-wasm package buildInputs |
| `crates/sunset-web-wasm/src/voice.rs` (new) | wasm-bindgen `voice_start` / `voice_stop` / `voice_input` methods + `VoiceState` holding encoder/decoder/loopback channel/handler |
| `crates/sunset-web-wasm/src/client.rs` (modify) | Wire the new voice methods into the `impl Client` block |
| `crates/sunset-web-wasm/src/lib.rs` (modify) | `mod voice;` |
| `crates/sunset-web-wasm/Cargo.toml` (modify) | Add `sunset-voice = { workspace = true }` |
| `web/audio/voice-capture-worklet.js` (new) | AudioWorkletProcessor that buffers 128-sample quanta into 960-sample frames and `postMessage`s them |
| `web/audio/voice-playback-worklet.js` (new) | AudioWorkletProcessor that receives 960-sample frames and writes them out as the audio engine pulls 128-sample quanta |
| `web/voice-demo.html` (new) | Throwaway demo page: `getUserMedia` → capture worklet → `client.voice_input` → loopback → handler → playback worklet → speakers. Manual verification only. |

The crate `sunset-voice` is intentionally **outside** `sunset-core` so non-voice consumers (TUI client, relay) don't drag in libopus.

---

## Verification strategy

- **Static**: `cargo build` on host + wasm32 for `sunset-voice` and `sunset-web-wasm`. `cargo clippy --workspace -- -D warnings`. `cargo fmt --all --check`.
- **Unit**: 7 cargo tests in `sunset-voice` (encoder construct, decoder construct, sine round-trip, silence round-trip, wrong frame size, empty packet, sequential frames independent).
- **Manual**: Open `web/voice-demo.html` in a browser, click Start, speak into mic, hear yourself with audible latency (~50–100 ms). Click Stop, audio cuts. This is C2a's acceptance test.

No automated browser test in C2a — that lands in C2c when the multi-peer mixer + jitter buffer make audio behaviour deterministic enough to assert programmatically.

---

## Task 1: Create empty `sunset-voice` crate skeleton

**Files:**
- Create: `crates/sunset-voice/Cargo.toml`
- Create: `crates/sunset-voice/src/lib.rs`
- Modify: `Cargo.toml` (workspace root)

**Why this task:** Pure boilerplate that gets the crate registered in the workspace before we touch libopus. Separates "does the workspace compile with a new empty crate" from "does libopus cross-compile to wasm32" (Task 2's risk).

- [ ] **Step 1: Create `crates/sunset-voice/Cargo.toml`**

```toml
[package]
name = "sunset-voice"
version.workspace = true
edition.workspace = true
license.workspace = true
rust-version.workspace = true

[lints]
workspace = true

[dependencies]
thiserror.workspace = true
```

(`opus` is intentionally absent in this task — added in Task 2.)

- [ ] **Step 2: Create `crates/sunset-voice/src/lib.rs` with constants only**

```rust
//! Voice codec wrappers (Opus) and audio constants for sunset.chat.
//!
//! Used by sunset-web-wasm's voice module for browser-side capture and
//! playback. Pure Rust — no JS, no wasm-bindgen, no Bus integration.
//! Networking and encryption land in C2b on top of this crate.

/// Sample rate used everywhere in the voice path. Opus's native rate
/// for VoIP; the browser AudioContext is created at this rate so we
/// never resample.
pub const SAMPLE_RATE: u32 = 48_000;

/// Mono. Voice doesn't benefit from stereo at the bandwidth budgets
/// we care about.
pub const CHANNELS: usize = 1;

/// Samples per 20 ms frame at 48 kHz mono. Opus's standard VoIP frame
/// duration; the audio worklet buffers 128-sample quanta into frames
/// of this size before handing them to the encoder.
pub const FRAME_SAMPLES: usize = 960;

/// Frame duration in milliseconds.
pub const FRAME_DURATION_MS: u32 = 20;
```

- [ ] **Step 3: Add the crate to the workspace members list**

In the root `Cargo.toml`, find the `members = [...]` line. Append `"crates/sunset-voice"` to the list (the existing line is one long string). The result should look like:

```toml
members = ["crates/sunset-store", "crates/sunset-store-memory", "crates/sunset-store-fs", "crates/sunset-sync", "crates/sunset-noise", "crates/sunset-core", "crates/sunset-core-wasm", "crates/sunset-sync-ws-native", "crates/sunset-sync-ws-browser", "crates/sunset-sync-webrtc-browser", "crates/sunset-web-wasm", "crates/sunset-relay", "crates/sunset-voice"]
```

- [ ] **Step 4: Add `sunset-voice` to `[workspace.dependencies]`**

In the root `Cargo.toml`, find the `[workspace.dependencies]` block and add (in alphabetical order):

```toml
sunset-voice = { path = "crates/sunset-voice" }
```

- [ ] **Step 5: Verify host build**

Run:

```bash
nix develop --command cargo build -p sunset-voice
```

Expected: `Finished` clean.

- [ ] **Step 6: Verify wasm32 build**

Run:

```bash
nix develop --command cargo build --target wasm32-unknown-unknown -p sunset-voice
```

Expected: `Finished` clean. (Trivial because there's no real code yet.)

- [ ] **Step 7: Commit**

```bash
git add crates/sunset-voice/Cargo.toml crates/sunset-voice/src/lib.rs Cargo.toml
git commit -m "Add empty sunset-voice crate scaffold

Constants only (SAMPLE_RATE, CHANNELS, FRAME_SAMPLES, FRAME_DURATION_MS).
Opus codec wrappers land in Task 2 once libopus cross-compile to
wasm32 is verified."
```

---

## Task 2: Add `opus` crate + libopus to Nix flake + verify wasm32 cross-compile

**Files:**
- Modify: `crates/sunset-voice/Cargo.toml`
- Modify: `flake.nix`

**Why this task:** This is the highest-risk task in C2a — getting libopus to cross-compile for `wasm32-unknown-unknown` is the unknown. We isolate it to a single task with explicit fallbacks documented before any other code depends on the codec.

The `opus` crate (https://crates.io/crates/opus) wraps libopus via pkg-config or `cc`. For wasm32 we need libopus headers and a precompiled library available to clang's wasm target. The Nix flake provides `pkgs.libopus`; we add it to `buildInputs` and rely on the `opus` crate's `pkg-config` discovery.

**Fallback paths if the wasm32 build fails (in order):**
1. Switch from `opus = "0.3"` to `audiopus = "0.3"` (sibling crate with active wasm32 support).
2. Vendor libopus source and build it via `cc::Build` in a custom `build.rs` targeting wasm32-unknown-unknown.
3. **Escalation**: if neither works after 4 hours of effort, stop and surface the problem to the user. Reverting the codec choice from Rust opus to JS WebCodecs is a brainstorm-level decision and not something to do unilaterally.

- [ ] **Step 1: Add `opus` dependency to `crates/sunset-voice/Cargo.toml`**

Edit `crates/sunset-voice/Cargo.toml` so the `[dependencies]` block reads:

```toml
[dependencies]
opus = "0.3"
thiserror.workspace = true
```

- [ ] **Step 2: Add libopus to the Nix flake's dev-shell buildInputs**

In `flake.nix`, find the dev-shell `buildInputs` block (the same block that includes `pkgs.cargo-watch`, `pkgs.cargo-nextest`, `pkgs.gleam`, etc.). Add:

```nix
pkgs.libopus
pkgs.pkg-config
```

(`pkg-config` is likely already present elsewhere — only add if missing. The reason it's needed in the dev shell is that the `opus` crate's build script uses `pkg-config` to locate libopus headers and library on the host target.)

For the `wasm32-unknown-unknown` target, libopus headers must also be visible to the cross-compiling clang. The `opus` crate tries pkg-config first; if that fails, it falls back to a vendored static build. Because `pkg-config` doesn't have a wasm32 mode out of the box, we expect the fallback path to fire automatically for wasm32 — libopus C source compiles via `cc` against the wasm32 target.

If the implementer finds that the `opus` crate doesn't ship libopus C source as a fallback (only pkg-config discovery), apply Fallback path 1 (switch to `audiopus`).

- [ ] **Step 3: Verify host build**

```bash
nix develop --command cargo build -p sunset-voice
```

Expected: `Finished` clean. The host build is the first sanity check — if this fails, libopus isn't visible to pkg-config in the dev shell.

- [ ] **Step 4: Verify wasm32 build (the risk moment)**

```bash
nix develop --command cargo build --target wasm32-unknown-unknown -p sunset-voice
```

Expected: `Finished` clean.

**If this step fails**, the implementer:
1. Reads the build error carefully (linker error vs. compile error vs. pkg-config error).
2. Tries Fallback path 1: edit `Cargo.toml` to use `audiopus = "0.3"` instead of `opus`. Re-run Step 4.
3. If that also fails, tries Fallback path 2: vendor libopus source under `crates/sunset-voice/vendor/libopus/` and write a custom `build.rs` invoking `cc::Build` with the wasm32 target. (The implementer is expected to know how to do this; it's standard cc-rs usage.)
4. If 4 hours of effort haven't yielded a clean wasm32 build, **stop and report BLOCKED**. The plan's codec choice may need revisiting.

- [ ] **Step 5: If you reached this step, both targets build. Commit.**

```bash
git add crates/sunset-voice/Cargo.toml flake.nix
git commit -m "Add opus crate dep + libopus to flake; verify cross-compile

Both 'cargo build -p sunset-voice' on host and on wasm32-unknown-unknown
target build cleanly. Codec wrappers land in Tasks 3 and 4."
```

If a fallback was applied, the commit message describes it (e.g. "Use audiopus instead of opus due to wasm32 build failure on opus crate").

---

## Task 3: Implement `VoiceEncoder` + unit tests

**Files:**
- Modify: `crates/sunset-voice/src/lib.rs`

**Why this task:** Encoder side. TDD: write the construction + frame-size validation tests first, then implement. One frame at a time, batch-style — caller gives exactly 960 samples, gets back encoded bytes.

- [ ] **Step 1: Add the failing tests + helper code in `crates/sunset-voice/src/lib.rs`**

Append to `crates/sunset-voice/src/lib.rs`:

```rust
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("opus error: {0}")]
    Opus(String),
    #[error("invalid frame size: expected {expected} samples, got {got}")]
    BadFrameSize { expected: usize, got: usize },
}

pub type Result<T> = std::result::Result<T, Error>;

/// Opus voice encoder configured for 48 kHz mono, 20 ms frames, VoIP
/// application, 24 kbit/s bitrate.
pub struct VoiceEncoder {
    inner: opus::Encoder,
}

impl VoiceEncoder {
    /// Construct a new encoder. Errors if libopus rejects the parameters
    /// (shouldn't happen with the constants we use).
    pub fn new() -> Result<Self> {
        let mut inner = opus::Encoder::new(SAMPLE_RATE, opus::Channels::Mono, opus::Application::Voip)
            .map_err(|e| Error::Opus(format!("encoder new: {e}")))?;
        inner
            .set_bitrate(opus::Bitrate::Bits(24_000))
            .map_err(|e| Error::Opus(format!("set_bitrate: {e}")))?;
        Ok(Self { inner })
    }

    /// Encode exactly one 20 ms frame (960 samples mono float). Returns
    /// the variable-length Opus packet bytes.
    pub fn encode(&mut self, pcm: &[f32]) -> Result<Vec<u8>> {
        if pcm.len() != FRAME_SAMPLES {
            return Err(Error::BadFrameSize {
                expected: FRAME_SAMPLES,
                got: pcm.len(),
            });
        }
        // Max Opus packet size is bounded by max bytes per second / frame
        // rate. 64 kbit/s * 20 ms = 160 bytes worst case; libopus says use
        // 4000 as a safe upper bound. We use 1500 — well above what 24
        // kbit/s ever produces.
        let mut out = vec![0u8; 1500];
        let n = self
            .inner
            .encode_float(pcm, &mut out)
            .map_err(|e| Error::Opus(format!("encode_float: {e}")))?;
        out.truncate(n);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encoder_constructs() {
        let enc = VoiceEncoder::new();
        assert!(enc.is_ok(), "encoder construction should succeed");
    }

    #[test]
    fn encode_wrong_frame_size_errors() {
        let mut enc = VoiceEncoder::new().unwrap();
        let result = enc.encode(&[0.0_f32; 480]);
        assert!(matches!(
            result,
            Err(Error::BadFrameSize { expected: 960, got: 480 })
        ));
    }

    #[test]
    fn encode_silence_produces_short_packet() {
        let mut enc = VoiceEncoder::new().unwrap();
        let bytes = enc.encode(&[0.0_f32; FRAME_SAMPLES]).unwrap();
        // Opus encodes silence very compactly (often <10 bytes).
        assert!(
            bytes.len() < 100,
            "silence packet should be small, got {} bytes",
            bytes.len()
        );
        assert!(!bytes.is_empty(), "packet should never be empty");
    }
}
```

- [ ] **Step 2: Run tests to verify they pass**

```bash
nix develop --command cargo test -p sunset-voice
```

Expected: 3 tests pass (`encoder_constructs`, `encode_wrong_frame_size_errors`, `encode_silence_produces_short_packet`).

If `encoder_constructs` fails on a libopus runtime error, that's a flake setup issue (libopus headers visible at compile time but library not linked at runtime). Re-check the flake `buildInputs` + library path.

- [ ] **Step 3: Verify wasm32 build**

```bash
nix develop --command cargo build --target wasm32-unknown-unknown -p sunset-voice
```

Expected: `Finished` clean.

- [ ] **Step 4: Commit**

```bash
git add crates/sunset-voice/src/lib.rs
git commit -m "sunset-voice: VoiceEncoder + unit tests

48 kHz mono, 20 ms frames, VoIP application, 24 kbit/s. Construction,
wrong-frame-size error, and silence-packet-shape tests pass on host;
wasm32 builds clean. Decoder + round-trip tests land in Task 4."
```

---

## Task 4: Implement `VoiceDecoder` + round-trip tests

**Files:**
- Modify: `crates/sunset-voice/src/lib.rs`

**Why this task:** Decoder side. The harder tests live here — round-trip energy preservation, silence preservation, sequential frame independence. These are the strongest evidence that the codec wrappers are wired correctly.

- [ ] **Step 1: Add the decoder type + new tests**

Append to `crates/sunset-voice/src/lib.rs`:

```rust
/// Opus voice decoder configured for 48 kHz mono.
pub struct VoiceDecoder {
    inner: opus::Decoder,
}

impl VoiceDecoder {
    pub fn new() -> Result<Self> {
        let inner = opus::Decoder::new(SAMPLE_RATE, opus::Channels::Mono)
            .map_err(|e| Error::Opus(format!("decoder new: {e}")))?;
        Ok(Self { inner })
    }

    /// Decode one Opus packet. Returns exactly 960 samples of mono
    /// float PCM (one 20 ms frame). The `fec` flag is false — we don't
    /// use forward error correction in C2a.
    pub fn decode(&mut self, opus_bytes: &[u8]) -> Result<Vec<f32>> {
        let mut out = vec![0.0_f32; FRAME_SAMPLES];
        let n = self
            .inner
            .decode_float(opus_bytes, &mut out, false)
            .map_err(|e| Error::Opus(format!("decode_float: {e}")))?;
        out.truncate(n);
        Ok(out)
    }
}
```

Then in the existing `mod tests` block, add these tests:

```rust
    #[test]
    fn decoder_constructs() {
        assert!(VoiceDecoder::new().is_ok());
    }

    #[test]
    fn decode_empty_packet_errors() {
        let mut dec = VoiceDecoder::new().unwrap();
        let err = dec.decode(&[]);
        assert!(
            matches!(err, Err(Error::Opus(_))),
            "empty packet should produce an Opus error, got {err:?}",
        );
    }

    #[test]
    fn round_trip_preserves_silence() {
        let mut enc = VoiceEncoder::new().unwrap();
        let mut dec = VoiceDecoder::new().unwrap();
        let silence = vec![0.0_f32; FRAME_SAMPLES];
        let bytes = enc.encode(&silence).unwrap();
        let decoded = dec.decode(&bytes).unwrap();
        assert_eq!(decoded.len(), FRAME_SAMPLES);
        let max_abs = decoded.iter().map(|s| s.abs()).fold(0.0_f32, f32::max);
        assert!(
            max_abs < 1e-3,
            "silence should decode close to zero; max |sample| = {max_abs}",
        );
    }

    #[test]
    fn round_trip_preserves_sine_energy() {
        let mut enc = VoiceEncoder::new().unwrap();
        let mut dec = VoiceDecoder::new().unwrap();
        // 440 Hz at 48 kHz, amplitude 0.5. Run several frames so
        // libopus has time to ramp up its internal state — Opus
        // typically suppresses the very first frame's transient.
        let mut decoded_rms = 0.0_f64;
        for _frame in 0..5 {
            let mut input = vec![0.0_f32; FRAME_SAMPLES];
            for (i, s) in input.iter_mut().enumerate() {
                let t = i as f32 / SAMPLE_RATE as f32;
                *s = 0.5 * (2.0 * std::f32::consts::PI * 440.0 * t).sin();
            }
            let bytes = enc.encode(&input).unwrap();
            let decoded = dec.decode(&bytes).unwrap();
            decoded_rms = (decoded.iter().map(|s| (*s as f64).powi(2)).sum::<f64>()
                / decoded.len() as f64)
                .sqrt();
        }
        // Input RMS is 0.5 / sqrt(2) ≈ 0.354. Decoded should be within
        // 20% — Opus is lossy but a steady sine in the speech band is
        // preserved well.
        let expected = 0.5_f64 / 2.0_f64.sqrt();
        let ratio = decoded_rms / expected;
        assert!(
            (0.8..=1.2).contains(&ratio),
            "decoded RMS {decoded_rms} (expected ≈ {expected}, ratio {ratio}) outside ±20%",
        );
    }

    #[test]
    fn sequential_frames_decode_independently() {
        let mut enc = VoiceEncoder::new().unwrap();
        let mut dec = VoiceDecoder::new().unwrap();
        // Encode three different sine wave frames at different
        // frequencies. Each decode should produce a non-empty,
        // 960-sample output, with energy in roughly the right range.
        // We don't assert exact spectrum — that would be flaky. Just
        // that the decoder keeps producing valid frames in sequence.
        for freq in [220.0_f32, 440.0, 880.0] {
            let mut input = vec![0.0_f32; FRAME_SAMPLES];
            for (i, s) in input.iter_mut().enumerate() {
                let t = i as f32 / SAMPLE_RATE as f32;
                *s = 0.3 * (2.0 * std::f32::consts::PI * freq * t).sin();
            }
            let bytes = enc.encode(&input).unwrap();
            let decoded = dec.decode(&bytes).unwrap();
            assert_eq!(decoded.len(), FRAME_SAMPLES);
            let max_abs = decoded.iter().map(|s| s.abs()).fold(0.0_f32, f32::max);
            assert!(
                max_abs > 0.05,
                "decoded frame at {freq} Hz should have audible energy",
            );
        }
    }
```

- [ ] **Step 2: Run tests**

```bash
nix develop --command cargo test -p sunset-voice
```

Expected: 7 tests pass (3 from Task 3 + 4 new).

If `round_trip_preserves_sine_energy` fails with the ratio outside ±20%, the codec parameters may be off (wrong sample rate, wrong channel count, wrong application mode). Recheck the encoder's constructor parameters against the spec.

- [ ] **Step 3: Verify wasm32 build**

```bash
nix develop --command cargo build --target wasm32-unknown-unknown -p sunset-voice
```

Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add crates/sunset-voice/src/lib.rs
git commit -m "sunset-voice: VoiceDecoder + round-trip tests

Round-trip preserves silence and sine wave energy within ±20%;
sequential frames at different frequencies decode independently.
Codec wrappers complete; wasm-bindgen surface lands in Task 5."
```

---

## Task 5: wasm-bindgen voice module on `Client`

**Files:**
- Create: `crates/sunset-web-wasm/src/voice.rs`
- Modify: `crates/sunset-web-wasm/src/lib.rs`
- Modify: `crates/sunset-web-wasm/src/client.rs`
- Modify: `crates/sunset-web-wasm/Cargo.toml`

**Why this task:** Expose the codec to JS through three small wasm-bindgen methods on `Client`: `voice_start(handler)`, `voice_stop()`, `voice_input(pcm)`. State (encoder, decoder, loopback channel, handler) lives behind an `Option<VoiceState>` on `Client`. The decode loop is a `spawn_local`-spawned task that drains the loopback channel, decodes each packet, and calls the registered JS handler.

- [ ] **Step 1: Add `sunset-voice` as a dependency of `sunset-web-wasm`**

In `crates/sunset-web-wasm/Cargo.toml`, find the `[dependencies]` block. Add (in alphabetical order):

```toml
sunset-voice = { workspace = true }
```

- [ ] **Step 2: Create `crates/sunset-web-wasm/src/voice.rs`**

```rust
//! wasm-bindgen surface for the voice pipeline (C2a).
//!
//! Three methods land on `Client`: `voice_start`, `voice_stop`,
//! `voice_input`. JS pushes mono 48 kHz PCM in via `voice_input`; Rust
//! encodes, in C2a routes through an in-process loopback queue, decodes,
//! and calls a registered JS handler with each decoded frame's PCM.
//!
//! In C2b the loopback queue is replaced by `Bus::publish_ephemeral`
//! (capture side) and `Bus::subscribe` (playback side). The wasm-bindgen
//! API surface here does not change.

use std::cell::RefCell;
use std::rc::Rc;

use js_sys::{Float32Array, Function};
use tokio::sync::mpsc;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::spawn_local;

use sunset_voice::{FRAME_SAMPLES, VoiceDecoder, VoiceEncoder};

/// Per-`Client` voice runtime state. `None` until `voice_start` is
/// called; cleared on `voice_stop`.
pub(crate) struct VoiceState {
    encoder: VoiceEncoder,
    /// Capture side: encoded bytes go in here.
    loopback_tx: mpsc::UnboundedSender<Vec<u8>>,
}

/// Inner data shared across `Client` for voice. Wrapped in
/// `Rc<RefCell<…>>` like the rest of `Client`'s mutable state.
pub(crate) type VoiceCell = Rc<RefCell<Option<VoiceState>>>;

pub(crate) fn new_voice_cell() -> VoiceCell {
    Rc::new(RefCell::new(None))
}

/// Start the voice subsystem. Spawns the loopback decode loop. The
/// `output_handler` is a JS callback invoked with a `Float32Array(960)`
/// for each decoded frame.
pub(crate) fn voice_start(state: &VoiceCell, output_handler: Function) -> Result<(), JsError> {
    let mut slot = state.borrow_mut();
    if slot.is_some() {
        return Err(JsError::new("voice already started"));
    }
    let encoder = VoiceEncoder::new().map_err(|e| JsError::new(&format!("{e}")))?;
    let mut decoder = VoiceDecoder::new().map_err(|e| JsError::new(&format!("{e}")))?;
    let (loopback_tx, mut loopback_rx) = mpsc::unbounded_channel::<Vec<u8>>();

    // Spawn the loopback decode loop. In C2b this is replaced by a
    // Bus subscribe loop, but the shape (decode + call JS handler) is
    // identical.
    spawn_local(async move {
        while let Some(bytes) = loopback_rx.recv().await {
            match decoder.decode(&bytes) {
                Ok(pcm) => {
                    let arr = Float32Array::from(pcm.as_slice());
                    // Ignore handler errors — JS-side issues shouldn't
                    // tear down the decoder.
                    let _ = output_handler.call1(&JsValue::NULL, &arr);
                }
                Err(_) => {
                    // Single-frame loss — log via console.warn and
                    // continue. C2c may add metrics here.
                    web_sys::console::warn_1(
                        &"sunset-voice: decode failed for one frame; dropped".into(),
                    );
                }
            }
        }
    });

    *slot = Some(VoiceState { encoder, loopback_tx });
    Ok(())
}

/// Stop the voice subsystem. Drops the encoder + loopback sender; the
/// decode loop exits when it next sees an empty channel.
pub(crate) fn voice_stop(state: &VoiceCell) -> Result<(), JsError> {
    *state.borrow_mut() = None;
    Ok(())
}

/// Submit one 20 ms frame of PCM. Length must be exactly 960.
pub(crate) fn voice_input(state: &VoiceCell, pcm: Float32Array) -> Result<(), JsError> {
    let mut slot = state.borrow_mut();
    let voice = slot.as_mut().ok_or_else(|| JsError::new("voice not started"))?;
    let len = pcm.length() as usize;
    if len != FRAME_SAMPLES {
        return Err(JsError::new(&format!(
            "voice_input expected {FRAME_SAMPLES} samples, got {len}"
        )));
    }
    let mut buf = vec![0.0_f32; FRAME_SAMPLES];
    pcm.copy_to(&mut buf);
    let encoded = voice
        .encoder
        .encode(&buf)
        .map_err(|e| JsError::new(&format!("{e}")))?;
    voice
        .loopback_tx
        .send(encoded)
        .map_err(|_| JsError::new("loopback channel closed"))?;
    Ok(())
}
```

- [ ] **Step 3: Add `mod voice;` in `crates/sunset-web-wasm/src/lib.rs`**

Find the existing `mod ...;` declarations in `lib.rs` (e.g. `mod client;`, `mod messages;`, etc.) and add `mod voice;` in alphabetical order.

- [ ] **Step 4: Wire the voice methods into `Client`**

This requires inspecting `crates/sunset-web-wasm/src/client.rs` to find:
- The `Client` struct definition (around line 38).
- The `impl Client` block (around line 51).

Read the file first to understand the current shape. The struct currently holds fields like the engine, identity, presence publisher, etc. The voice state needs to be added alongside.

In the `Client` struct, add the field:

```rust
    voice: voice::VoiceCell,
```

In the `Client::new` constructor (the `#[wasm_bindgen(constructor)] pub fn new(...)` method), where the other fields are initialised, add:

```rust
            voice: voice::new_voice_cell(),
```

In the `impl Client` block (the `#[wasm_bindgen] impl Client { ... }` block — the one that already contains `add_relay`, `connect_direct`, `send_message`, etc.), add three new methods. Place them after the existing message-related methods for grouping:

```rust
    /// Initialise the voice subsystem. Must be called before
    /// `voice_input`. Spawns an in-process loopback decode loop;
    /// `output_handler` is invoked with a Float32Array(960) for each
    /// decoded 20 ms frame.
    pub fn voice_start(&self, output_handler: js_sys::Function) -> Result<(), JsError> {
        voice::voice_start(&self.voice, output_handler)
    }

    /// Stop the voice subsystem and release its resources.
    pub fn voice_stop(&self) -> Result<(), JsError> {
        voice::voice_stop(&self.voice)
    }

    /// Submit one 20 ms frame of mono PCM (Float32Array of length 960
    /// at 48 kHz) for encoding + loopback delivery to the output handler.
    pub fn voice_input(&self, pcm: js_sys::Float32Array) -> Result<(), JsError> {
        voice::voice_input(&self.voice, pcm)
    }
```

- [ ] **Step 5: Build wasm32**

```bash
nix develop --command cargo build --target wasm32-unknown-unknown -p sunset-web-wasm
```

Expected: `Finished` clean.

- [ ] **Step 6: Build host**

```bash
nix develop --command cargo build -p sunset-web-wasm
```

Expected: `Finished` clean. (sunset-web-wasm is wasm-targeted but should still compile on host for IDE / lint purposes.)

- [ ] **Step 7: Commit**

```bash
git add crates/sunset-web-wasm/Cargo.toml crates/sunset-web-wasm/src/voice.rs crates/sunset-web-wasm/src/lib.rs crates/sunset-web-wasm/src/client.rs
git commit -m "wasm-wasm: voice_start/voice_stop/voice_input on Client

Three wasm-bindgen methods expose the C2a audio pipeline: JS pushes
PCM via voice_input, Rust encodes, an in-process loopback channel
feeds a spawn_local decode loop, decoded frames go to a JS handler
registered at voice_start. The encoded Opus bytes never touch JS.
In C2b the loopback channel is replaced by Bus publish/subscribe."
```

---

## Task 6: Audio worklet JS files

**Files:**
- Create: `web/audio/voice-capture-worklet.js`
- Create: `web/audio/voice-playback-worklet.js`

**Why this task:** Two small, self-contained AudioWorkletProcessor JS files. They run on the audio thread and bridge between the audio engine's 128-sample quanta and our 960-sample (20 ms) frames. No Rust involvement; pure JS.

- [ ] **Step 1: Create `web/audio/voice-capture-worklet.js`**

```js
// AudioWorkletProcessor that buffers raw 128-sample quanta from the
// browser's audio engine into 960-sample (20 ms) mono frames and
// posts each completed frame to the main thread.
//
// Used by web/voice-demo.html (C2a) and the eventual production wiring
// (C2c). The main thread receives Float32Array(960) and forwards them
// to client.voice_input.

class VoiceCaptureProcessor extends AudioWorkletProcessor {
  constructor() {
    super();
    this.buf = new Float32Array(960);
    this.idx = 0;
  }

  process(inputs) {
    // First input, first channel (mono). May be undefined briefly
    // during stream start/end transitions.
    const ch = inputs[0]?.[0];
    if (!ch) return true;

    let i = 0;
    while (i < ch.length) {
      const room = 960 - this.idx;
      const take = Math.min(room, ch.length - i);
      this.buf.set(ch.subarray(i, i + take), this.idx);
      this.idx += take;
      i += take;

      if (this.idx === 960) {
        // Transfer ownership of the buffer to avoid a copy. Allocate
        // a fresh one for the next frame.
        const out = this.buf;
        this.port.postMessage(out, [out.buffer]);
        this.buf = new Float32Array(960);
        this.idx = 0;
      }
    }
    return true;
  }
}

registerProcessor("voice-capture", VoiceCaptureProcessor);
```

- [ ] **Step 2: Create `web/audio/voice-playback-worklet.js`**

```js
// AudioWorkletProcessor that receives 960-sample (20 ms) mono PCM
// frames via postMessage, queues them, and writes them out into the
// audio engine's 128-sample quanta as the rendering pipeline pulls.
//
// Underflow (queue empty when the engine pulls) is silenced with
// zeros for the missing samples. C2c may add a jitter buffer; C2a
// accepts dropouts as audible feedback that something is misaligned.

class VoicePlaybackProcessor extends AudioWorkletProcessor {
  constructor() {
    super();
    this.queue = []; // array of Float32Array
    this.head = null;
    this.headIdx = 0;
    this.port.onmessage = (e) => {
      // Defensive: only accept Float32Arrays of the expected length.
      if (e.data instanceof Float32Array && e.data.length === 960) {
        this.queue.push(e.data);
      }
    };
  }

  process(_inputs, outputs) {
    const out = outputs[0]?.[0];
    if (!out) return true;

    let i = 0;
    while (i < out.length) {
      if (!this.head) {
        if (this.queue.length === 0) {
          // Underflow — pad the rest of this quantum with silence.
          out.fill(0, i);
          return true;
        }
        this.head = this.queue.shift();
        this.headIdx = 0;
      }
      const remaining = this.head.length - this.headIdx;
      const take = Math.min(remaining, out.length - i);
      out.set(this.head.subarray(this.headIdx, this.headIdx + take), i);
      this.headIdx += take;
      i += take;
      if (this.headIdx === this.head.length) {
        this.head = null;
      }
    }
    return true;
  }
}

registerProcessor("voice-playback", VoicePlaybackProcessor);
```

- [ ] **Step 3: Verify the files are syntactically valid JS**

The build pipeline doesn't lint these (worklet files are loaded directly by the browser). A quick sanity check:

```bash
nix develop --command node --check web/audio/voice-capture-worklet.js
```

Expected: no output (silent success). `node --check` parses without executing.

(Note: `node --check` will warn that `AudioWorkletProcessor` and `registerProcessor` are undefined globals. That's fine — they're injected by the AudioWorklet runtime in the browser. We're only checking syntax.)

```bash
nix develop --command node --check web/audio/voice-playback-worklet.js
```

Expected: same.

- [ ] **Step 4: Commit**

```bash
git add web/audio/voice-capture-worklet.js web/audio/voice-playback-worklet.js
git commit -m "Add capture + playback AudioWorkletProcessor JS files

Capture worklet buffers 128-sample audio quanta into 960-sample (20 ms)
mono frames and postMessages them. Playback worklet receives frames
via postMessage, queues them, and writes them into the audio engine's
output quanta. Underflow is silenced with zeros."
```

---

## Task 7: Demo page + manual verification

**Files:**
- Create: `web/voice-demo.html`

**Why this task:** End-to-end manual test of the audio pipeline. A standalone HTML page wires `getUserMedia` through the capture worklet, into `client.voice_input`, through the in-Rust loopback, out via the registered handler, into the playback worklet, and to the speakers. Speaking into the mic and hearing yourself is C2a's acceptance test.

The page is intentionally minimal — no Gleam, no UI framework. Hand-written HTML + JS that imports the wasm-bindgen bundle directly.

- [ ] **Step 1: Inspect how the wasm bundle is currently served**

Read `web/playwright.config.js` and look for the static-server config + `web/build/dev/javascript/sunset_web_wasm.js`. Confirm:

```bash
ls web/build/dev/javascript/sunset_web_wasm*.js 2>/dev/null
ls web/static-web-server.toml 2>/dev/null || ls web/dev-server*.json 2>/dev/null
```

The Bus + Plan A integration tests already serve static assets out of `web/`; the demo page goes alongside `web/index.html` (or wherever the existing entry point is). If the existing layout puts the wasm bundle at `/sunset_web_wasm.js`, the demo's import path mirrors that.

- [ ] **Step 2: Create `web/voice-demo.html`**

```html
<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8" />
    <title>sunset-voice loopback demo (C2a)</title>
    <style>
      body {
        font-family: system-ui, sans-serif;
        max-width: 32rem;
        margin: 2rem auto;
        padding: 0 1rem;
      }
      button {
        padding: 0.5rem 1rem;
        margin-right: 0.5rem;
        font-size: 1rem;
      }
      #log {
        margin-top: 1rem;
        font-family: ui-monospace, monospace;
        font-size: 0.85rem;
        white-space: pre-wrap;
        max-height: 20rem;
        overflow: auto;
        background: #f4f4f4;
        padding: 0.5rem;
      }
    </style>
  </head>
  <body>
    <h1>sunset-voice loopback (C2a)</h1>
    <p>
      Mic → Opus encode → in-Rust loopback queue → Opus decode → speakers.
      Speak into the mic; you should hear yourself with audible latency
      (~50–100 ms). No networking, no encryption.
    </p>
    <button id="start">Start</button>
    <button id="stop" disabled>Stop</button>
    <div id="log"></div>

    <script type="module">
      import init, { Client } from "./build/dev/javascript/sunset_web_wasm.js";

      const log = (msg) => {
        const el = document.getElementById("log");
        el.textContent += msg + "\n";
        el.scrollTop = el.scrollHeight;
      };

      let client = null;
      let audioCtx = null;
      let captureNode = null;
      let playbackNode = null;
      let micStream = null;

      async function start() {
        document.getElementById("start").disabled = true;
        try {
          // 1. WASM bootstrap.
          await init();
          log("wasm initialised");

          // 2. Construct a Client. The room name is irrelevant for
          //    C2a — voice doesn't touch Bus yet.
          const seed = new Uint8Array(32);
          crypto.getRandomValues(seed);
          client = new Client(seed, "voice-demo");
          log("client constructed");

          // 3. Get the mic at 48 kHz mono with browser-side cleanup.
          micStream = await navigator.mediaDevices.getUserMedia({
            audio: {
              channelCount: 1,
              sampleRate: 48000,
              echoCancellation: true,
              noiseSuppression: true,
              autoGainControl: true,
            },
          });
          log("mic acquired");

          // 4. AudioContext at 48 kHz so we never resample.
          audioCtx = new AudioContext({ sampleRate: 48000 });
          await audioCtx.audioWorklet.addModule("audio/voice-capture-worklet.js");
          await audioCtx.audioWorklet.addModule("audio/voice-playback-worklet.js");
          log("worklets loaded");

          // 5. Build the playback node FIRST so we can hand its port to
          //    the voice_start handler.
          playbackNode = new AudioWorkletNode(audioCtx, "voice-playback", {
            numberOfInputs: 0,
            numberOfOutputs: 1,
            outputChannelCount: [1],
          });
          playbackNode.connect(audioCtx.destination);

          // 6. Voice subsystem: Rust will call this handler with each
          //    decoded frame; we forward to the playback worklet.
          client.voice_start((pcm) => {
            playbackNode.port.postMessage(pcm);
          });
          log("voice_start ok");

          // 7. Capture node: receives 960-sample frames via its port,
          //    forwards each to client.voice_input.
          captureNode = new AudioWorkletNode(audioCtx, "voice-capture", {
            numberOfInputs: 1,
            numberOfOutputs: 0,
          });
          captureNode.port.onmessage = (e) => {
            try {
              client.voice_input(e.data);
            } catch (err) {
              log("voice_input error: " + err);
            }
          };

          // 8. Wire mic → capture worklet.
          const source = audioCtx.createMediaStreamSource(micStream);
          source.connect(captureNode);
          log("audio pipeline live — speak into the mic");

          document.getElementById("stop").disabled = false;
        } catch (err) {
          log("start failed: " + err);
          document.getElementById("start").disabled = false;
        }
      }

      async function stop() {
        document.getElementById("stop").disabled = true;
        try {
          if (client) {
            client.voice_stop();
            log("voice_stop ok");
          }
          if (captureNode) {
            captureNode.disconnect();
            captureNode = null;
          }
          if (playbackNode) {
            playbackNode.disconnect();
            playbackNode = null;
          }
          if (audioCtx) {
            await audioCtx.close();
            audioCtx = null;
          }
          if (micStream) {
            for (const t of micStream.getTracks()) t.stop();
            micStream = null;
          }
          client = null;
          log("stopped");
        } catch (err) {
          log("stop failed: " + err);
        }
        document.getElementById("start").disabled = false;
      }

      document.getElementById("start").addEventListener("click", start);
      document.getElementById("stop").addEventListener("click", stop);
    </script>
  </body>
</html>
```

The import path `./build/dev/javascript/sunset_web_wasm.js` matches the existing serving layout; if the implementer finds the wasm bundle is served from a different path, adjust the import accordingly.

- [ ] **Step 3: Build the wasm bundle so the demo has something to load**

```bash
nix develop --command cargo build --target wasm32-unknown-unknown -p sunset-web-wasm --release
nix develop --command bash -c '
  mkdir -p web/build/dev/javascript &&
  wasm-bindgen \
    --target web \
    --no-typescript \
    --out-dir web/build/dev/javascript \
    --out-name sunset_web_wasm \
    target/wasm32-unknown-unknown/release/sunset_web_wasm.wasm
'
```

Expected: `web/build/dev/javascript/sunset_web_wasm.js` and `sunset_web_wasm_bg.wasm` produced.

- [ ] **Step 4: Manual verification (the C2a acceptance test)**

Start the existing static server (or any static server pointed at `web/`):

```bash
nix develop --command bash -c 'cd web && bunx static-web-server -p 8080 -d .'
```

(Or whatever the existing project convention is for serving web/ — `bun run dev`, `npm start`, etc. Look at `web/package.json` `scripts` for the right command.)

Open `http://localhost:8080/voice-demo.html` in a browser. Click **Start**. Grant microphone permission when prompted.

**Acceptance criteria:**
1. The log panel shows "audio pipeline live — speak into the mic".
2. Speaking into the mic produces audible playback through speakers/headphones with ~50–100 ms latency.
3. Silence produces silence (no constant background tone or buzz).
4. Clicking **Stop** cuts the audio cleanly. Clicking **Start** again restarts everything.

If any of these fails, the implementer reports `BLOCKED` with which step failed and what was logged.

- [ ] **Step 5: Commit**

```bash
git add web/voice-demo.html
git commit -m "Add C2a voice loopback demo page

Standalone HTML page that wires getUserMedia through the capture
worklet, into client.voice_input, through the in-Rust loopback queue,
out via the voice_start handler, into the playback worklet, to the
speakers. Manual verification (speak → hear yourself) is C2a's
acceptance test; automated browser tests land in C2c."
```

---

## Task 8: Lint, format, and full workspace build

**Files:** No code changes; this task verifies the workspace is clean.

- [ ] **Step 1: Clippy on host target**

```bash
nix develop --command cargo clippy -p sunset-voice --all-targets -- -D warnings
nix develop --command cargo clippy -p sunset-web-wasm --all-targets -- -D warnings
```

Expected: exit 0 each. Fix any warnings inline before committing.

- [ ] **Step 2: Clippy on wasm32 target**

```bash
nix develop --command cargo clippy --target wasm32-unknown-unknown -p sunset-voice -- -D warnings
nix develop --command cargo clippy --target wasm32-unknown-unknown -p sunset-web-wasm -- -D warnings
```

Expected: exit 0 each.

- [ ] **Step 3: Workspace clippy**

```bash
nix develop --command cargo clippy --workspace --all-features --all-targets -- -D warnings
```

Expected: exit 0. Confirms no downstream crate broke from the new `sunset-voice` member or the `sunset-web-wasm` voice module.

- [ ] **Step 4: cargo fmt check**

```bash
nix develop --command cargo fmt --all --check
```

Expected: exit 0. If drift, run `nix develop --command cargo fmt --all` and commit as a separate commit.

- [ ] **Step 5: Full workspace test**

```bash
nix develop --command cargo test --workspace --all-features
```

Expected: all tests pass. The 7 new sunset-voice tests appear; nothing else should have regressed.

- [ ] **Step 6: Commit fmt drift if any**

If Step 4 produced changes:

```bash
git add -u
git commit -m "fmt: apply rustfmt after voice pipeline implementation"
```

---

## Spec coverage check (self-review)

| Spec section / requirement | Implemented in |
|---|---|
| New `sunset-voice` crate at `crates/sunset-voice` | Task 1 (skeleton); Task 2 (opus dep); Tasks 3-4 (encoder/decoder) |
| Audio constants (`SAMPLE_RATE`, `CHANNELS`, `FRAME_SAMPLES`, `FRAME_DURATION_MS`) | Task 1 |
| `VoiceEncoder` with `new()` + `encode(pcm)` | Task 3 |
| `VoiceDecoder` with `new()` + `decode(opus_bytes)` | Task 4 |
| `Error` enum (Opus, BadFrameSize) + `Result` alias | Task 3 |
| 7 unit tests covering encode/decode, round-trip, errors | Tasks 3 + 4 |
| libopus added to Nix flake; cross-compile verified | Task 2 (with explicit fallback paths) |
| `voice` module on `sunset-web-wasm` | Task 5 |
| `Client::voice_start` / `voice_stop` / `voice_input` wasm-bindgen methods | Task 5 |
| In-Rust loopback queue connecting encoder to decoder | Task 5 |
| `spawn_local` decode loop calling JS handler | Task 5 |
| Capture AudioWorkletProcessor (128-quanta → 960-frame buffering) | Task 6 |
| Playback AudioWorkletProcessor (960-frame → 128-quanta with underflow=silence) | Task 6 |
| Demo page wiring `getUserMedia` through the full pipeline | Task 7 |
| Manual acceptance test (speak → hear yourself) | Task 7 Step 4 |
| Lint clean (host + wasm32) | Task 8 |
| Fmt clean | Task 8 |
| Encoded Opus bytes never touch JS | Task 5 design (loopback channel and handler are both Rust-side; only PCM crosses the wasm-bindgen boundary) |
| No networking, no Bus, no Room, no Liveness | Plan does not modify any of those crates; sunset-web-wasm only adds the new `voice` module and three methods on `Client` |

Self-review: every spec requirement maps to a concrete task. No placeholders. Type names (`VoiceEncoder`, `VoiceDecoder`, `Error`, `VoiceState`, `voice_start`, `voice_stop`, `voice_input`, `FRAME_SAMPLES`, `SAMPLE_RATE`) are consistent across all tasks.

One known unknown explicitly called out: the libopus wasm32 cross-compile mechanism in Task 2 (with documented fallback paths and an escalation criterion).

---

## Done criteria

- [ ] Task 1 commit landed: empty `sunset-voice` crate scaffold.
- [ ] Task 2 commit landed: `opus` dep + libopus in flake; wasm32 cross-compile verified.
- [ ] Task 3 commit landed: `VoiceEncoder` + 3 unit tests.
- [ ] Task 4 commit landed: `VoiceDecoder` + 4 round-trip tests.
- [ ] Task 5 commit landed: `voice_start` / `voice_stop` / `voice_input` on `Client`.
- [ ] Task 6 commit landed: capture + playback worklet JS files.
- [ ] Task 7 commit landed: demo HTML page; manual acceptance test passed (speak → hear yourself).
- [ ] Task 8: clippy clean (host + wasm32 + workspace), fmt clean, full workspace tests pass.
- [ ] Spec coverage table fully checked.
