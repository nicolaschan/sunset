# sunset-voice: Audio Pipeline Design (C2a)

**Date:** 2026-04-28
**Scope:** First slice of the voice work. Pure Rust audio pipeline (mic → Opus encode → bytes → Opus decode → speaker) with no networking and no UI integration. Tested in isolation via cargo unit tests for the codec and a manual loopback browser demo for the audio bridge.
**Predecessors:** Bus pub/sub, WebRTC unreliable datachannel (Plan A), Liveness layer (Plan C1) — all merged.
**Successors:**
- **C2b**: VoiceFrame format, `Room` encryption, single-peer round-trip across the network.
- **C2c**: Multi-peer mixing, jitter buffer, wire the existing Gleam UI controls (volume / denoise / deafen / join / leave / mute) to real Rust state.

## Goal

Get sound out of speakers. End to end inside one browser tab: real microphone capture → real speaker playback, with a complete Opus encode + decode in between. No networking yet — encoded bytes are loopback-routed through an in-Rust queue, not through `Bus`. After C2a we know the audio path works end-to-end before we layer encryption, networking, or mixing on top.

## Non-goals (deferred to C2b/C2c)

- Networking. No `Bus::publish_ephemeral`, no `Room::encrypt`, no peer-to-peer plumbing.
- Multiple peers. C2a has exactly one encoder and one decoder; the loopback queue is point-to-point.
- Jitter buffer, packet loss concealment, FEC. The loopback queue is in-process and doesn't drop bytes.
- VAD / DTX / silence frames. C2a continuously encodes whatever the mic produces.
- UI controls (volume, denoise, deafen, mute, join/leave). The voice worklet runs as soon as `voice_start()` is called; there's no UI surface yet.
- AGC, echo cancellation, denoise. We rely on the browser's `getUserMedia` constraints (`echoCancellation: true`, `noiseSuppression: true`, `autoGainControl: true`) and don't add any DSP of our own. Denoise as a per-peer toggle is a C2c concern.
- Push-to-talk. Always-on while active.
- Native targets (TUI, relay). The codec library compiles for native too, but no audio I/O integration outside the browser.

## Architecture

The audio path inside one tab:

```
┌──────────────────────────────────────────────────────────────────┐
│ JS / browser                                                      │
│                                                                    │
│  getUserMedia → MediaStreamSource ──► capture-worklet (audio thread)
│                                          │ buffers 128-sample quanta
│                                          │ into 960-sample (20ms) frames
│                                          ▼ postMessage(Float32Array)
│                                       main thread
│                                          │
│                                          ▼ client.voice_input(pcm)
└──────────────────────────────────────────┼───────────────────────┘
                                           │
                                           ▼ wasm-bindgen FFI
┌──────────────────────────────────────────────────────────────────┐
│ Rust / WASM                                                       │
│                                                                    │
│  voice_input(pcm)                                                  │
│    └─ encoder.encode(pcm) → Vec<u8>                                │
│       └─ loopback.send(bytes)            ← C2a only; C2b replaces  │
│                                            this with Bus publish    │
│                                                                    │
│  loopback recv-loop (spawn_local task)                             │
│    └─ decoder.decode(bytes) → Vec<f32>                             │
│       └─ output_handler(pcm)   ← JS callback                        │
└──────────────────────────────────────────┼───────────────────────┘
                                           │
                                           ▼ JS callback
┌──────────────────────────────────────────────────────────────────┐
│ JS / browser                                                      │
│                                                                    │
│  output_handler(pcm) ──► postMessage(Float32Array)                 │
│                            ▼                                        │
│                          playback-worklet (audio thread)           │
│                            │ writes into AudioContext destination   │
│                            ▼                                        │
│                          speakers                                   │
└──────────────────────────────────────────────────────────────────┘
```

The encoded Opus bytes **never touch JS**. JS pushes PCM in via `voice_input`, JS receives PCM out via `output_handler`. Everything in between (encode, loopback, decode, and in C2b: encrypt, Bus publish, network, Bus subscribe, decrypt) is one Rust call chain.

## Audio configuration

All choices are standard VoIP defaults. No per-deployment knobs in C2a.

| Parameter | Value | Why |
|---|---|---|
| Sample rate | **48 kHz** | Opus's native rate. Browser AudioContext is created with `{ sampleRate: 48000 }` so we never need to resample. |
| Channels | **Mono** | Stereo for voice doubles bandwidth for negligible UX gain. |
| Frame size | **20 ms (960 samples)** | Standard VoIP cadence. Opus supports 2.5/5/10/20/40/60 ms; 20 ms is the universal sweet spot for latency vs. compression. |
| Bitrate | **24 kbit/s** (Opus default for `Application::Voip`) | Good speech quality at low bandwidth. Tunable later. |
| Opus application | `Application::Voip` | Tuned for speech rather than music. |
| Complexity | 5 (Opus default) | Balanced encode-side CPU vs. quality. |

## Components

### `crates/sunset-voice` (new crate)

A new top-level crate. Pure Rust opus encoder + decoder wrappers over the `opus` crate (which wraps libopus). Lives outside `sunset-core` so non-voice consumers (TUI client, relay) don't drag in libopus.

**Cargo deps:**

```toml
[dependencies]
opus = "0.3"   # wraps libopus; cross-compiles to wasm32 with libopus available at build time
thiserror.workspace = true
```

The Nix flake gains libopus as a build input for both host and `wasm32-unknown-unknown` cross-compilation. The exact mechanism (vendored libopus C source built via `cc` crate, vs. precompiled libopus wasm artifact) is determined during implementation; if it turns out non-trivial, the implementation plan documents the fallback.

**Public API:**

```rust
pub struct VoiceEncoder { /* opus::Encoder + state */ }
pub struct VoiceDecoder { /* opus::Decoder + state */ }

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("opus error: {0}")]
    Opus(String),
    #[error("invalid frame size: expected {expected} samples, got {got}")]
    BadFrameSize { expected: usize, got: usize },
}

pub type Result<T> = std::result::Result<T, Error>;

impl VoiceEncoder {
    /// 48 kHz mono, 20 ms frames, VoIP application, 24 kbit/s.
    pub fn new() -> Result<Self>;
    /// Encode exactly one 20 ms frame (960 samples). Returns encoded
    /// Opus bytes (variable-length per frame).
    pub fn encode(&mut self, pcm: &[f32]) -> Result<Vec<u8>>;
}

impl VoiceDecoder {
    /// 48 kHz mono.
    pub fn new() -> Result<Self>;
    /// Decode one Opus packet. Returns 960 samples of PCM (one 20 ms frame).
    pub fn decode(&mut self, opus: &[u8]) -> Result<Vec<f32>>;
}
```

The API is intentionally **batch-style with fixed frame size**: the caller must supply exactly 960 samples per `encode` call, and `decode` always returns 960 samples. Frame alignment is the caller's job (the audio worklet does the buffering); the codec doesn't carry partial-frame state.

**Constants exposed:**

```rust
pub const SAMPLE_RATE: u32 = 48_000;
pub const CHANNELS: usize = 1;
pub const FRAME_SAMPLES: usize = 960;          // 20 ms at 48 kHz
pub const FRAME_DURATION_MS: u32 = 20;
```

These are consumed by the audio worklet (via wasm-bindgen exports) and by the loopback / future `Room` framing layer.

### `crates/sunset-web-wasm/src/voice.rs` (new module)

Wasm-bindgen surface that JS calls. Two methods on `Client`:

```rust
#[wasm_bindgen]
impl Client {
    /// Initialise the voice subsystem. Must be called before
    /// `voice_input`. Spawns the loopback decode loop. The handler
    /// is called on the main thread for each decoded 20 ms frame.
    pub fn voice_start(&self, output_handler: js_sys::Function) -> Result<(), JsError>;

    /// Stop the voice subsystem. Drops the encoder, decoder, and
    /// loopback queue. After `voice_stop`, calling `voice_input` is
    /// an error until `voice_start` is called again.
    pub fn voice_stop(&self) -> Result<(), JsError>;

    /// Submit one 20 ms frame of PCM (Float32Array of length 960,
    /// mono, 48 kHz) to be encoded and (in C2a) routed through the
    /// loopback queue. In C2b this fan-outs to Bus::publish_ephemeral.
    pub fn voice_input(&self, pcm: js_sys::Float32Array) -> Result<(), JsError>;
}
```

The `output_handler` is a JS callback that receives a `Float32Array(960)` for each decoded frame. The wasm side calls it via `Function::call1`. JS forwards the array to the playback worklet via `postMessage`.

State lives on `Client`: an `Option<VoiceState>` that holds the encoder, decoder, loopback channel, and registered handler. `voice_start` populates it, `voice_stop` clears it.

### `web/audio/voice-capture-worklet.js` (new file)

A small AudioWorkletProcessor that:

1. Receives 128-sample quanta from the audio rendering pipeline.
2. Accumulates them into a 960-sample (20 ms) buffer.
3. When the buffer is full, `postMessage`s a `Float32Array(960)` to the main thread and resets.

```js
class VoiceCaptureProcessor extends AudioWorkletProcessor {
  constructor() {
    super();
    this.buf = new Float32Array(960);
    this.idx = 0;
  }
  process(inputs) {
    const ch = inputs[0]?.[0];  // first input, first channel (mono)
    if (!ch) return true;
    let i = 0;
    while (i < ch.length) {
      const room = 960 - this.idx;
      const take = Math.min(room, ch.length - i);
      this.buf.set(ch.subarray(i, i + take), this.idx);
      this.idx += take;
      i += take;
      if (this.idx === 960) {
        // Transfer ownership to avoid a copy.
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

### `web/audio/voice-playback-worklet.js` (new file)

The mirror image: receives `Float32Array(960)` via `postMessage`, queues them, and writes them out into `outputs[0][0]` as the audio engine pulls 128-sample quanta.

```js
class VoicePlaybackProcessor extends AudioWorkletProcessor {
  constructor() {
    super();
    this.queue = [];   // array of Float32Array
    this.head = null;
    this.headIdx = 0;
    this.port.onmessage = (e) => this.queue.push(e.data);
  }
  process(_, outputs) {
    const out = outputs[0]?.[0];
    if (!out) return true;
    let i = 0;
    while (i < out.length) {
      if (!this.head) {
        if (this.queue.length === 0) {
          // Underflow — pad with silence. C2c may add a jitter buffer.
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

### `web/src/sunset_web/voice_demo.gleam` or `web/src/voice-demo.html` (one of these — TBD during implementation, see "Demo wiring" below)

A throwaway debug page that:

1. Calls `getUserMedia({ audio: { echoCancellation: true, noiseSuppression: true, autoGainControl: true, channelCount: 1 } })`.
2. Creates an `AudioContext({ sampleRate: 48000 })`.
3. Loads both worklets via `audioContext.audioWorklet.addModule(...)`.
4. Wires: `MediaStreamSource → AudioWorkletNode("voice-capture")`. The node's port forwards each 960-sample message to `client.voice_input(pcm)`.
5. Calls `client.voice_start(out_pcm => { playbackNode.port.postMessage(out_pcm); })`.
6. Wires: `AudioWorkletNode("voice-playback") → audioContext.destination`.
7. Adds Start / Stop buttons.

The page lives outside the main app (no Gleam UI integration — that's C2c). Manual verification only.

### Loopback queue (Rust internal, C2a-only)

Inside `voice.rs`, the encoder pushes encoded bytes into a `mpsc::UnboundedSender<Vec<u8>>`. A `spawn_local` task runs a loop reading from the receiver, decoding each packet, and calling the JS handler. In C2b this entire loopback is replaced by `Bus::publish_ephemeral` (encoder side) and `Bus::subscribe` (decoder side). The Rust API surface (`voice_input`, `voice_start`, `voice_stop`) does not change — only the middle gets swapped.

## Data flow

**Capture path:**
1. `getUserMedia` returns a `MediaStream` with one mono audio track at 48 kHz.
2. `MediaStreamSource` feeds into `AudioWorkletNode("voice-capture")`.
3. The capture worklet buffers 128-sample quanta into 960-sample frames.
4. Each frame is `postMessage`d (with transfer) to the main thread.
5. Main thread calls `client.voice_input(pcm)`.
6. Rust encodes via `VoiceEncoder::encode`.
7. Encoded bytes go into the C2a loopback channel.

**Playback path:**
1. Loopback decode task pulls encoded bytes, calls `VoiceDecoder::decode`.
2. Decoded `Vec<f32>` is converted to `js_sys::Float32Array` and passed to the registered handler.
3. JS handler `postMessage`s the array to `AudioWorkletNode("voice-playback")`.
4. Playback worklet enqueues, then writes samples into `outputs[0][0]` as the audio engine pulls them.
5. `AudioContext.destination` plays through speakers.

## Failure modes

| Scenario | Behaviour |
|---|---|
| `getUserMedia` denied by user | The demo page surfaces the error from the JS Promise rejection. C2a doesn't define how Gleam UI surfaces this — that's C2c. |
| `voice_input` called before `voice_start` | Returns an error to JS. JS demo logs it. |
| `voice_input` called with wrong frame size (≠ 960 samples) | Encoder returns `Error::BadFrameSize`; wasm-bindgen propagates as `JsError`. The capture worklet should never produce wrong sizes; this is a defensive check. |
| Encoder transient failure (libopus internal error) | Returns `Error::Opus`; the frame is dropped. C2a logs and continues — single-frame loss is recoverable. |
| Decoder transient failure | Same as encoder: drop the frame, log, continue. |
| Loopback queue overflow | `mpsc::UnboundedSender` has no overflow path. C2a's loopback runs at the same realtime cadence as capture and should not back up; if it does, the decode task is starving for some reason and we have a bigger problem than buffering. |
| Audio context suspended (autoplay policy, page hidden) | The capture and playback worklets stop running; `voice_input` is never called. Resuming the AudioContext (via user gesture) restores everything without restarting the codec. |
| Playback queue underflow (decode task slower than playback) | Playback worklet pads with silence (zeros) for the missing samples. Audible as a dropout. C2c jitter buffer addresses this; C2a accepts dropouts as visible-feedback that something's off. |

## Testing strategy

### Unit tests (`crates/sunset-voice`)

1. **Encoder constructs.** `VoiceEncoder::new()` succeeds with the configured parameters.
2. **Decoder constructs.** Same.
3. **Round-trip preserves energy on a sine wave.** Generate 960 samples of a 440 Hz sine wave at 48 kHz, encode, decode, compute RMS of input and output. Decoded RMS should be within ±20% of input RMS (Opus is lossy but speech-band sine waves survive cleanly).
4. **Round-trip preserves silence.** Encode 960 zeros, decode, assert all output samples are within `1e-3` of 0. (Opus may emit tiny non-zero values for silence; tolerance covers that.)
5. **Wrong frame size errors.** `encode(&[0.0; 480])` returns `BadFrameSize { expected: 960, got: 480 }`.
6. **Empty packet errors.** `decode(&[])` returns an `Opus` error (or whatever libopus produces — exact error type pinned during implementation).
7. **Sequential frames decode independently.** Encode three different sine wave frames (different frequencies); decode each independently; each decoded RMS matches expectation. Establishes that the codec doesn't carry hidden state across frame boundaries in a way that breaks our per-frame batch model.

These run with `cargo test -p sunset-voice` on the host target. **No browser involvement.**

### Browser loopback demo (manual verification)

The throwaway demo page is the C2a acceptance test. The success criteria, performed by a human:

1. Open the demo page in a browser.
2. Click "Start". Grant mic permission.
3. Speak into the mic.
4. Hear yourself, with audible latency (~50–100 ms is expected end-to-end with capture buffering, codec, and playback queueing).
5. Click "Stop". Verify the audio cuts.

This is not automated. Browser audio testing with Playwright is possible (`--use-fake-device-for-media-stream` + `--use-file-for-fake-audio-capture`) but the verification ("does the output sound right?") is hard to assert programmatically without a substantial setup. C2a's automated coverage is the cargo unit tests; the browser demo is a manual gate.

### Out of scope

- **End-to-end Playwright voice test.** Deferred — likely lands in C2c after the multi-peer mixer + jitter buffer make audio quality predictable enough to write deterministic assertions.

## Out of scope

- VoiceFrame format (postcard struct with `sender_time`, `opus_bytes`). C2b.
- `Room::encrypt_voice` / `Room::decrypt_voice` helpers. C2b.
- Bus integration. C2b.
- Liveness integration. C2b — the consumer side of Bus subscribe pipes into `Liveness::observe_event`.
- Multi-peer mixing. C2c. C2a is one-encoder-one-decoder.
- Jitter buffer. C2c.
- UI control wiring (volume, denoise, deafen, mute, join/leave). C2c. The existing `voice_minibar.gleam` and per-peer popover stay on mock data through C2a and C2b.
- VAD / DTX. C2c at earliest; possibly never if always-on works fine.
- Echo cancellation, AGC, noise suppression — relied on from `getUserMedia` constraints, no Rust-side DSP.
- Native (TUI / relay) audio. The codec compiles native; nobody calls it on native targets.

## Risks

1. **libopus cross-compile to wasm32.** This is the highest-risk piece. The `opus` crate's build.rs invokes a C compiler against the libopus source. For wasm32 we need a cross-compile toolchain (likely `cc` with the right target triple, or precompiled wasm artifacts via `wasm-pack` toolchain). The Nix flake will need libopus and possibly emscripten-style headers. **Mitigation**: implementation plan's Task 1 is exactly this — get libopus building inside `nix develop` for `cargo build --target wasm32-unknown-unknown -p sunset-voice`. If it turns out infeasible, fallback options (in order of preference): (a) build libopus separately and link via `cc` crate manually; (b) try `audiopus` instead of `opus`; (c) escalate and reconsider Plan A from the previous brainstorm (WebCodecs).
2. **AudioWorklet quirks across browsers.** Worklet processor lifecycle (especially around `AudioContext.suspend()` from autoplay policy) varies subtly. **Mitigation**: the demo page works on Firefox and Chromium; Safari (which has different worklet quirks) is verified during C2c when more of the UI is in place.
3. **Echo from speakers back into mic.** With `echoCancellation: true` in the constraints, `getUserMedia` runs the browser's echo canceller. For a single-tab loopback test this should work; for cross-peer voice (C2b+) it'll be a real test of the canceller. C2a accepts whatever `getUserMedia` provides.
4. **Latency from main-thread hop.** ~10 ms added by capture-worklet → main → wasm → loopback → main → playback-worklet. Acceptable for C2a's purpose (audible feedback). If C2c needs tighter latency we can revisit the worklet-loaded-WASM bridge architecture.
5. **`spawn_local` from a non-LocalSet context.** `wasm-bindgen-futures::spawn_local` requires a JS event loop, which is always present in the browser, so this is fine for the WASM target. The codec crate itself doesn't spawn anything — only the wasm-wasm wrapper does, in `voice_start`.
6. **The `opus` crate's API surface.** The crate's encoder takes `Application::Voip / Audio / LowDelay`. C2a uses `Voip`. If the crate's API turns out to be inconvenient, switching to `audiopus` is straightforward — the wrapper layer in `sunset-voice` insulates downstream consumers.

## Demo wiring

The throwaway demo page lives somewhere outside the main Gleam app. Two options, decided during implementation (no architectural impact):

- **A**: Add it as a Gleam module that's only loaded under a query string flag (e.g. `?voice-demo=1`). Reuses the existing build pipeline.
- **B**: A standalone `web/voice-demo.html` served alongside the main app, hand-written JS, no Gleam involvement. Decouples the demo from the Gleam app entirely; easier to delete after C2a.

B is simpler and aligns with "throwaway." Default to B unless there's a reason to share Gleam state.

## Review summary

- **Placeholders:** The exact libopus build mechanism is acknowledged as "determined during implementation" — this is not a placeholder, it's a known unknown that the plan resolves with a real Task 1. The demo wiring is similarly noted as A-vs-B with B as default.
- **Internal consistency:** The architecture diagram, component list, data flow, and failure modes all describe the same one-direction-PCM-only JS↔Rust boundary with the codec entirely in Rust. The 20 ms / 48 kHz / mono parameters are consistent across all sections.
- **Scope:** Strictly bounded to the audio pipeline. Networking, UI, multi-peer, and jitter are explicitly enumerated as out-of-scope and pointed at C2b / C2c.
- **Ambiguity:** "Loopback queue" is defined explicitly as an in-Rust mpsc channel that gets replaced by Bus in C2b. The capture / playback worklets handle 128-sample quanta vs 960-sample frames; both paths are spelled out concretely.
