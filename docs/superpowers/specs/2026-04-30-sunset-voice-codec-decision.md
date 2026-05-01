# Voice Codec Decision Record

**Date:** 2026-04-30
**Scope:** Records what we tried for the C2a voice codec, why we shipped the passthrough placeholder, and what to revisit when picking a production codec in C2b.
**Supersedes the codec-related sections of:** `docs/superpowers/specs/2026-04-28-sunset-voice-pipeline-design.md` (which committed to libopus). The audio-bridge architecture in that spec is unchanged.

## Where we landed

`sunset-voice::VoiceEncoder` / `VoiceDecoder` are a **passthrough** today:

- `encode(pcm: &[f32])` writes the f32 samples out as little-endian bytes (`FRAME_SAMPLES * 4` per frame).
- `decode(bytes: &[u8])` reads them back.
- No compression, no resampling, no codec dependency.

This is *not* a production-suitable wire format — at 48 kHz mono it's about 1.5 Mbps per peer per direction, ~60× higher than Opus at the same quality. It's a placeholder that:

1. Validates the audio bridge end-to-end (mic → AudioWorklet → main thread → wasm-bindgen → Rust mpsc → wasm-bindgen → main thread → AudioWorklet → speakers).
2. Keeps the `VoiceEncoder` / `VoiceDecoder` abstraction in place so a real codec slots in without touching `sunset-web-wasm` or any consumer.
3. Unblocks C2b (network + encryption) work, which will then make a more informed codec choice.

## What we tried first

### Attempt 1: libopus C source via cc-rs (Rust-side codec)

**Plan:** vendor libopus C source under `crates/sunset-voice/vendor/libopus/`, build it via `cc::Build` in `sunset-voice/build.rs`, expose `VoiceEncoder` / `VoiceDecoder` over hand-written `extern "C"` FFI bindings.

**What worked:**
- The C source compiled cleanly for `wasm32-unknown-unknown` via clang.
- The static archive `libopus.a` was produced at the expected output path.
- `cargo:rustc-link-lib=static=opus` was emitted by the build script (verified in `target/.../sunset-voice-*/output`).
- Host-target unit tests passed (8 round-trip tests including silence preservation and sine-wave RMS within ±20%).

**What broke:**
- The final wasm bundle had `env` imports for every `opus_*` function we declared via FFI. Verified by parsing the wasm import section directly off disk.
- The reason (verified via `cargo build -vv`): rustc's command line to `rust-lld` had `-L native=...` for the libopus.a search path, but **no `-l opus`**. The `cargo:rustc-link-lib=static=opus` directive emitted by `sunset-voice`'s build script was not propagating to the downstream `sunset-web-wasm` cdylib's link step.

**What we tried to fix the propagation:**
- `links = "opus"` in `sunset-voice/Cargo.toml` to mark the crate as linking a native lib. No effect.
- `cargo:rustc-link-lib=static:+whole-archive=opus` (force the linker to include all symbols regardless of references). Triggered "Bitcode section not found in object file" — `+whole-archive` enables Rust's LTO bitcode check and fails on plain C objects.
- Suppressing cc::Build's auto-emit (`build.cargo_metadata(false)`) and re-emitting the directive ourselves with `+whole-archive`. Same LTO error.
- Raw linker args `-Wl,--undefined=opus_*` and bare `--undefined=opus_*` via `cargo:rustc-link-arg-cdylib`. Cargo silently ignored the arg-cdylib variant when emitted from a non-cdylib package; bare `--undefined` reached wasm-ld but didn't pull symbols from a static archive that wasn't on the link line in the first place.
- Adding a separate `build.rs` to `sunset-web-wasm` that re-emitted `cargo:rustc-link-lib=static=opus`. The directive went into the build-script output file but still didn't reach the rustc command line.

**Why we stopped:** four hours into Task 2's escalation budget, with the hours-deep symptom being *"why does Cargo not propagate `cargo:rustc-link-lib=static=...` from a transitive dep's build.rs to a downstream cdylib's link?"* — a question whose answer is upstream Cargo behavior, not anything we'd discover by adding more code. See "Things to revisit" below.

### Attempt 2: WebCodecs `AudioEncoder` / `AudioDecoder` (browser-side codec)

**Plan:** stop trying to ship libopus, use the browser's WebCodecs API. `sunset-voice` keeps the API surface; `sunset-web-wasm/src/voice.rs` invokes WebCodecs through `web-sys`. Encoded `EncodedAudioChunk` bytes flow through the same loopback channel.

**What worked:**
- The wasm bundle built cleanly after enabling `--cfg=web_sys_unstable_apis` in `.cargo/config.toml` (web-sys gates WebCodecs behind that flag in addition to the per-type feature flags).
- All the wasm-bindgen surface lined up; Client exported `voice_start` / `voice_stop` / `voice_input`.

**What broke:**
- Firefox 2026 doesn't support `AudioEncoder` for Opus encoding. (Decoder yes, encoder no. Same browser, same WebCodecs implementation, asymmetric coverage.)

**Why we stopped:** firefox-only-half-the-pipeline isn't shippable. We could have switched to a hybrid (Chrome encodes, Firefox falls back to raw PCM, decode works everywhere) but that's a significant complexity increase for a transitional choice and starts mixing codec choice into runtime detection.

### Attempt 3: passthrough (what shipped)

**Plan:** drop the codec from C2a entirely. `VoiceEncoder` / `VoiceDecoder` are byte-for-byte passthroughs. Audio bridge gets validated; codec decision moves to C2b where it's evaluated alongside the wire format / network / encryption work.

**What worked:** everything. Demo plays the user's voice back in the browser within ~80 ms.

## Things to revisit when we pick the production codec (C2b)

In rough order of how I'd lean today:

1. **Pure-Rust Opus port.** `audiopus` (the `_sys` crate this depends on) failed for us via CMake; but other crates exist (`opus-native`, possibly `magnum-opus` ports). Avoids both the libopus link issue and the browser-API support gap. Maturity unknown — needs investigation. If a viable pure-Rust port exists, it's the cleanest answer and gives us the same codec on every target (web, future TUI, future Minecraft mod).

2. **libopus, take 2 — but co-located with the cdylib.** The Cargo behavior we hit suggests `cargo:rustc-link-lib=...` from a build.rs in an `rlib` dependency doesn't propagate to a downstream `cdylib`'s link step. This deserves an upstream Cargo issue search/file. **The workaround we didn't fully try:** put libopus's build.rs *inside* the cdylib package (`sunset-web-wasm`) instead of `sunset-voice`. Cargo would then emit `-l static=opus` directly from the cdylib's own build script, no propagation needed. That requires moving the vendored C source either into `sunset-web-wasm` or into a workspace location both crates can read; it's not architecturally clean but it might just work.

3. **WebCodecs Opus, hybrid.** Use `AudioEncoder.isConfigSupported({ codec: "opus", ... })` as a feature detector. On Chrome / Edge / Safari (whenever Safari adds it), use WebCodecs. On Firefox, fall back to either pure-Rust Opus or passthrough. The decoder side works in Firefox today, so receive-only voice ("you can hear them; they can't hear you") is at least possible while the encoder gap is open.

4. **Hand off to ffmpeg.wasm or similar.** Heavyweight (~10MB+ wasm). Last resort.

5. **Stay on passthrough.** Acceptable for trusted-LAN deployments and dev environments. Not acceptable for general internet use because of bandwidth (~1.5 Mbps per direction).

## Migration mechanics (when the time comes)

The single edit point is `crates/sunset-voice/src/lib.rs`. Everything above the `VoiceEncoder` / `VoiceDecoder` API treats encoded bytes as opaque. Concretely the swap involves:

1. Replace the body of `VoiceEncoder::encode` and `VoiceDecoder::decode` with the new codec.
2. Add the `CODEC_ID` constant for the new codec (`"opus"` if it's Opus, etc.) — used in C2b's `VoiceFrame` postcard struct so the receive side knows what to feed its decoder.
3. The encoded byte size changes. `PASSTHROUGH_ENCODED_BYTES` becomes inappropriate as a constant; nothing currently consumes it, but the C2b `VoiceFrame` format should be variable-length-friendly from day one to accommodate this.
4. Tests: replace `round_trip_sine_is_bit_exact` with the same `round_trip_preserves_sine_energy` (RMS within ±20%) we wrote during the libopus attempt. Add a frozen-vector test for the encoded-byte format if the codec produces stable output (Opus does, with `Application::Voip` and a fixed bitrate).

The wasm-bindgen layer in `sunset-web-wasm/src/voice.rs` does not need to change. Neither does the demo HTML, the worklets, or anything in Gleam.

## Open questions for C2b's brainstorm

- Does the wire format need a per-frame codec ID, or do we negotiate codec at session start?
- How do peers signal supported codecs? (Probably an extension to the existing presence / room-membership announcements.)
- Bitrate adaptation: do we need it for v1, or is a fixed bitrate (24 kbps) good enough?
- Forward error correction: Opus has built-in FEC; if we switch codecs the C2b voice frame format may need a separate FEC field.

These don't need answers now. The passthrough placeholder doesn't make any of them harder.
