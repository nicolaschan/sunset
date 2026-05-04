# Sunset Voice C2c Design — Multi-peer voice in the Gleam UI

**Date:** 2026-05-03
**Scope:** Take voice from "two browsers in a test harness" to "real Gleam UI, multi-peer, controllable, end-to-end tested." Lift the protocol logic out of `sunset-web-wasm` into a host-agnostic `VoiceRuntime` in `sunset-voice` so future native clients (TUI, relay, mobile) reuse it. Wire join/leave, mic mute, deafen, and per-peer volume to real Rust state. Auto-connect the WebRTC mesh on join. Per-peer playback worklets (browser sums). Land an honest e2e test suite (2-way, 3-way, churn, mute/deafen, mic permission) that catches both connectivity bugs and content bugs (silent / repeated / wrong-peer frames).

**Out of scope (deferred):** real opus encode/decode (codec stays passthrough; libopus WASM cross-compile is its own plan), real-DSP per-peer denoise (UI toggle stays cosmetic), cross-browser Playwright (Chromium only), network-degradation tests, voice-channel-as-distinct-from-room (one voice channel per room in v1), explicit `Leave` packet (membership_liveness expiry inside the 6 s budget is enough).

**Builds on:**
- C2a `sunset-voice` codec abstraction (`VoiceEncoder` / `VoiceDecoder` passthrough).
- C2a audio bridge in `sunset-web-wasm/src/voice/` (mic worklet → wasm; wasm → speaker worklet).
- C2b encrypted `VoicePacket` over `Bus`, per-peer `Liveness`, `voice_start`/`voice_input`/`voice_stop` FFI.
- Multi-room `RoomHandle` (#16) — `voice_start(room, ...)`, `connect_direct` lives on `RoomHandle`.
- Plan A WebRTC unreliable datachannel.
- C1 `sunset-core::Liveness`.
- `sunset-core::Bus` + `BusImpl`.
- `sunset-sync::PeerSupervisor`.

## Goal

A real user opens sunset.chat, joins a room, clicks the voice channel, and within a couple of seconds is hearing other people in that room — and they hear them. They can mute their mic, deafen themselves, adjust per-peer volume. They see who's in the call, who's talking, who's muted. People joining and leaving the call work the way they expect (no stuck "ghost" peers, no need to rejoin). Three-way calls work. The Playwright suite proves it — including content checks that catch silent-audio and stuck-frame regressions.

## Architecture

```
       Gleam UI (Lustre)
       ┌──────────────────────────────────────────────────┐
       │ Click voice channel → join/leave                 │
       │ Mic icon → toggle mute                           │
       │ Headphones icon → toggle deafen                  │
       │ Per-peer volume slider, talking lights, muted    │
       │ icon, in_call indicators                         │
       └────────────┬──────────────────────┬──────────────┘
                    │ FFI                  │ event callbacks
                    v                      ^
       ┌──────────────────────────────────────────────────┐
       │ sunset-web-wasm/voice (THIN SHELL — browser glue)│
       │  - audio capture + playback worklet integration  │
       │  - per-peer GainNode for volume                  │
       │  - JS<->WASM marshalling for FFI                 │
       │  - Dialer impl wrapping RoomHandle               │
       │  - FrameSink impl pushing to per-peer worklets   │
       └────────────┬─────────────────────────────────────┘
                    │
       ┌────────────v─────────────────────────────────────┐
       │ sunset-voice::VoiceRuntime (PROTOCOL LOGIC)      │
       │  - heartbeat task (carries is_muted)             │
       │  - subscribe loop → decrypt → per-peer jitter    │
       │  - liveness combiner → PeerStateSink             │
       │  - auto-connect FSM (calls Dialer)               │
       │  - jitter buffer + pump task                     │
       │  - mute / deafen state                           │
       └────────────┬─────────────────────────────────────┘
                    │
       Bus / Room / Identity (host-supplied)
```

Two parallel rules of placement:

1. **Anything specific to "this is a browser"** (audio worklets, GainNode, JS callbacks, wasm-bindgen) lives in `sunset-web-wasm/voice/`.
2. **Anything that future TUI / native / mobile clients will also need** (heartbeat cadence, subscribe + decrypt, Liveness combination, auto-dial orchestration, jitter buffer, mute/deafen state) lives in `sunset-voice::VoiceRuntime`.

## Crate split

### `sunset-voice` (extended)

Already contains: `VoicePacket`, `EncryptedVoicePacket`, encrypt/decrypt, `VoiceEncoder`/`VoiceDecoder` (passthrough), `derive_voice_key`.

C2c adds:

- `pub struct VoiceRuntime` — owns all protocol state.
- `pub trait Dialer` — host-supplied, "ensure direct connection to peer X exists."
- `pub trait FrameSink` — host-supplied, receives decoded PCM ready to play.
- `pub trait PeerStateSink` — host-supplied, receives `VoicePeerState` change events.
- `pub struct VoiceTasks` — bag of `LocalBoxFuture<'static, ()>` for the host to spawn (heartbeat, subscribe, combiner, auto-connect, jitter pump).
- `pub struct VoicePeerState { peer, in_call, talking, is_muted }`.

`VoiceRuntime` is `?Send` (single-threaded, matches the project's WASM constraint). It does not call `tokio::spawn`. The host calls `wasm_bindgen_futures::spawn_local` (browser) or `tokio::task::LocalSet::spawn_local` (native) on each future returned in `VoiceTasks`.

### `sunset-web-wasm/voice/` (collapses)

Today this directory contains the protocol logic that's being lifted out (`subscriber.rs`, `transport.rs`, `liveness.rs`). After C2c it contains only:

- `mod.rs` — FFI entry points (`voice_start`, `voice_stop`, `voice_input`, `voice_set_muted`, `voice_set_deafened`).
- `audio.rs` — capture/playback worklet integration (the existing JS bridge).
- `dialer.rs` — `Dialer` impl wrapping `RoomHandle::connect_direct`.
- `frame_sink.rs` — `FrameSink` impl that pushes PCM to a per-peer playback worklet via `postMessage`, and manages the per-peer `GainNode` table.
- `peer_state_sink.rs` — `PeerStateSink` impl that calls the JS `on_voice_peer_state` callback.

Net effect: `sunset-web-wasm/voice/` shrinks; `sunset-voice` grows; the protocol is testable in isolation in pure Rust.

### Future native client (out of scope, but informs the design)

`sunset-tui-voice` (or wherever native audio I/O lands) will provide its own `Dialer`/`FrameSink`/`PeerStateSink` implementations (CPAL or similar for audio I/O, native dial mechanism), construct a `VoiceRuntime`, and spawn the futures with `tokio::task::LocalSet::spawn_local`. No changes to `sunset-voice` will be needed.

## Wire format

`VoicePacket::Heartbeat` gains an `is_muted` field. Since voice has not shipped, no migration story is required — the frozen test vector for the wire format is updated.

```rust
#[derive(Serialize, Deserialize)]
pub enum VoicePacket {
    Frame { codec_id: String, seq: u64, sender_time_ms: u64, payload: Vec<u8> },
    Heartbeat { sent_at_ms: u64, is_muted: bool },
}
```

`Frame` is unchanged. Encryption (`EncryptedVoicePacket`, AAD binding, namespace `voice/<room_fp>/<sender>`) is unchanged from C2b.

`VoicePeerState` (the runtime's emitted event) gains `is_muted`:

```rust
pub struct VoicePeerState {
    pub peer: PeerId,
    pub in_call: bool,
    pub talking: bool,
    pub is_muted: bool,
}
```

Combiner emits a state change when any of `(in_call, talking, is_muted)` differs from the last emitted value for that peer (debounce stays).

## `VoiceRuntime` API

### Traits

```rust
#[async_trait(?Send)]
pub trait Dialer {
    /// Idempotent: dial peer if no direct connection exists yet. Returns
    /// immediately. Connection establishment is async; observe completion
    /// via PeerSupervisor's connection-state stream (out of scope here).
    async fn ensure_direct(&self, peer: PeerId);
}

pub trait FrameSink {
    /// Called once per (peer, jitter-buffer pop). PCM is FRAME_SAMPLES
    /// (960) f32 mono @ 48kHz. Browser impl posts to per-peer playback
    /// worklet via postMessage.
    fn deliver(&self, peer: &PeerId, pcm: &[f32]);

    /// Peer is leaving the audio mesh — release per-peer playback
    /// resources (worklet node, GainNode, output channel). Fired once
    /// when a peer transitions to Gone (membership_liveness Stale).
    fn drop_peer(&self, peer: &PeerId);
}

pub trait PeerStateSink {
    fn emit(&self, state: &VoicePeerState);
}
```

### Construction and tasks

```rust
pub struct VoiceRuntime { /* opaque */ }

pub struct VoiceTasks {
    pub heartbeat: LocalBoxFuture<'static, ()>,
    pub subscribe: LocalBoxFuture<'static, ()>,
    pub combiner: LocalBoxFuture<'static, ()>,
    pub auto_connect: LocalBoxFuture<'static, ()>,
    pub jitter_pump: LocalBoxFuture<'static, ()>,
}

impl VoiceRuntime {
    pub fn new(
        bus: Arc<dyn Bus>,
        room: Rc<Room>,
        identity: Identity,
        dialer: Rc<dyn Dialer>,
        frame_sink: Rc<dyn FrameSink>,
        peer_state_sink: Rc<dyn PeerStateSink>,
    ) -> (Self, VoiceTasks);

    /// Capture path entry — encodes and publishes the frame, gated by mute.
    pub fn send_pcm(&self, pcm: &[f32]);

    pub fn set_muted(&self, muted: bool);
    pub fn set_deafened(&self, deafened: bool);

    /// Drop on VoiceRuntime cancels all task futures (each future awaits
    /// a Weak it can't upgrade once VoiceRuntime is dropped, then exits).
    pub fn stop(self);
}
```

The host pattern:

```rust
let (runtime, tasks) = VoiceRuntime::new(bus, room, identity, dialer, frame_sink, peer_state_sink);
spawn_local(tasks.heartbeat);
spawn_local(tasks.subscribe);
spawn_local(tasks.combiner);
spawn_local(tasks.auto_connect);
spawn_local(tasks.jitter_pump);
// runtime is held by the host; dropping it cancels everything.
```

### Concurrency cancellation

Each task's future captures a `Weak<RuntimeInner>`. On every loop iteration, the task tries `weak.upgrade()`; if it fails, the task returns. `VoiceRuntime` holds the only strong `Rc<RuntimeInner>`, so dropping it terminates all five tasks within one iteration of each.

## Auto-connect FSM

Per peer, the `auto_connect` task observes membership_liveness events and runs this state machine:

```
            ┌──────────────────────┐
            │      Unknown         │ — never seen this peer's heartbeat
            └──────────┬───────────┘
                       │ first heartbeat received
                       v
            ┌──────────────────────┐
            │     Dialing          │ — called dialer.ensure_direct(peer)
            └──────────┬───────────┘
                       │ heartbeat continues — no extra dial calls
                       v
            ┌──────────────────────┐
            │     Connected        │ — frames flowing (or expected to)
            └──────────┬───────────┘
                       │ membership_liveness Stale (no heartbeat for 5s)
                       v
            ┌──────────────────────┐
            │      Gone            │ → call frame_sink.drop_peer
            └──────────────────────┘   (re-enters Unknown if heartbeat returns)
```

`Connected` is a logical state — the FSM doesn't actually verify the WebRTC connection is open. `PeerSupervisor` handles that (and its own retry). The FSM only tracks "have I asked for a connection to this peer yet, since the last time I considered them gone." That's enough: `Dialer::ensure_direct` is idempotent, `PeerSupervisor` retries with backoff, and if a peer is unreachable they stay "Dialing" forever (which is fine — their `talking` light just never lights up, which honestly reflects the situation).

**Revision (Phase 1.5):** Auto-connect bootstraps off a separate durable "voice-presence" signal published on `voice-presence/<room_fp>/<sender>` with TTL ~6 s and refresh ~2 s. Voice heartbeats are reserved for post-bootstrap `in_call` / `talking` indicators only; they would otherwise fail to propagate over the WebSocket relay (which drops unreliable traffic). The voice-presence publisher is added to `VoiceTasks`. Drop-on-departure still flows from `membership_liveness` Stale via heartbeats over the established WebRTC connection.

**Revision (Phase 1.6):** Two additional fixes were required to make Phase 1.5 actually work end-to-end:

1. **Engine: union of own filters in SUBSCRIBE_NAME entry.** The relay's `SubscriptionRegistry` is `HashMap<VerifyingKey, Filter>` — one filter per peer. Before this fix, calling `engine.publish_subscription(filter)` more than once (which happens whenever a client subscribes to multiple namespaces — chat `<fp>/`, voice frames `voice/<fp>/`, voice-presence `voice-presence/<fp>/`) silently overwrote the prior filter at the relay. This broke not just voice but also WebRTC signaling (under `<fp>/webrtc/`), because once voice subscribed to `voice/<fp>/`, the relay no longer matched `<fp>/webrtc/...` for that peer and dropped signaling messages on the floor — so `connect_direct` hung forever waiting for an Answer that the relay had refused to forward. Fix: `SyncEngine` accumulates an in-memory `own_filters: Vec<Filter>` and writes the union as the single SUBSCRIBE_NAME entry on every `publish_subscription` call. Single-element case still emits the filter directly (no Union wrapper) so the wire format stays identical for one-subsystem clients. Unsubscription is not supported in v1; the union grows monotonically per-engine.

2. **Voice auto-connect: glare avoidance.** With both peers running auto-connect off the same voice-presence signal, both would call `Dialer::ensure_direct` simultaneously. The browser WebRTC transport handles glare by ignoring duplicate Offers from a peer it's already mid-handshake with — which drops the *initiator-side* Offer too, leaving each side with one connect-side handshake waiting for an Answer that's been suppressed and one accept-side handshake answering the peer's Offer. The two independently-derived `RTCPeerConnection`s then race ICE/SCTP setup and neither completes within the test/UX budget. Fix: only the lexicographically smaller pubkey side initiates the dial; the other side's auto-connect FSM defers to its accept path. This is the cheapest possible tiebreak — no negotiation rounds, no clocks, no state. A future Perfect Negotiation implementation could replace the asymmetry with proper rollback semantics.

**Revision (Phase 6.1, post-implementation review):** Per-peer playback volume management does **not** flow through the Rust FFI. The original spec showed `Client::voice_set_peer_volume(peer_id, gain)` (and an `on_set_peer_volume` JS callback registered at `voice_start`) on the assumption that the runtime should own the desired-gain table and replay queued values to late-allocated GainNodes. In practice the GainNode is a fundamentally browser-shaped concept — it's allocated by `voice.ffi.mjs::deliverFrame` on the first frame from a peer, lives in the per-peer `{ worklet, gain }` table on the JS side, and has no native counterpart any non-browser host would share. Threading the value through Rust adds a function-pointer hop and a `RefCell<HashMap>` of pending gains for zero functional benefit. The Gleam UI calls `voice.ffi.mjs::setPeerVolume(peerHex, gain)` directly; if the GainNode isn't allocated yet, the call is dropped on the floor (the volume slider is rendered only for peers already in the popover, which means the UI has already heard from them and the slot exists). Future native hosts (TUI, Minecraft mod) that grow per-peer volume will define their own host-shaped surface; nothing in the protocol layer cares. This drops `Client::voice_set_peer_volume`, the `on_set_peer_volume` callback parameter on `voice_start`, and the `pending_gains` map.

## Jitter buffer

Per peer, single FIFO of decoded PCM frames (`VecDeque<Vec<f32>>`).

| Parameter | Value | Note |
|---|---|---|
| Target depth | 4 frames (80 ms) | Push side may exceed when bursty |
| Max depth | 8 frames (160 ms) | On overflow, drop oldest |
| Pump cadence | 20 ms | Matches FRAME_DURATION |
| Underrun action | Repeat last delivered frame once, then deliver silence (zeros) until a new frame arrives | Matches today's playback worklet PLC |

Reordering is not handled in v1. The unreliable WebRTC datachannel is in fact unordered, so frames *can* arrive out of order, but in practice they rarely do over short paths. Frames feed the buffer in arrival order. The `seq` field is *not* used by the jitter buffer for ordering; it is only used by tests (and future v2 reorder-aware buffers) to detect violations.

**Deafen interaction:** when deafened, the jitter pump still runs (so liveness stays accurate), but it skips `FrameSink::deliver` calls. Internal buffer state stays consistent so un-deafening resumes mid-stream cleanly.

**Mute interaction:** mute is on the *send* side. The jitter buffer is unaffected.

## FFI surface (`sunset-web-wasm`)

```rust
impl Client {
    pub fn voice_start(&self, room: &str, callbacks: &VoiceCallbacks) -> Result<(), JsError>;
    pub fn voice_stop(&self) -> Result<(), JsError>;
    pub fn voice_input(&self, pcm: &Float32Array) -> Result<(), JsError>;

    pub fn voice_set_muted(&self, muted: bool);
    pub fn voice_set_deafened(&self, deafened: bool);
    // Per-peer volume is intentionally JS-only — see Phase 6.1 revision above.

    // Test hooks — compiled in only with feature `test-hooks`.
    #[cfg(feature = "test-hooks")]
    pub fn voice_inject_pcm(&self, pcm: &Float32Array);
    #[cfg(feature = "test-hooks")]
    pub fn voice_install_frame_recorder(&self);
    #[cfg(feature = "test-hooks")]
    pub fn voice_recorded_frames(&self, peer_id: &Uint8Array) -> JsValue;
    #[cfg(feature = "test-hooks")]
    pub fn voice_active_peers(&self) -> JsValue;  // [{peer_id, in_call, talking, is_muted}]
}

#[wasm_bindgen]
pub struct VoiceCallbacks {
    pub on_voice_peer_state: Function, // (peer_id: Uint8Array, in_call, talking, is_muted)
    // No on_frame callback in C2c — frames go directly from VoiceRuntime
    // through FrameSink → per-peer playback worklet, never to JS user code.
}
```

Behaviour:

- `voice_start`: creates the `Dialer`/`FrameSink`/`PeerStateSink` adapters, constructs `VoiceRuntime`, spawns its five tasks via `spawn_local`. Initiates `getUserMedia`; on user denial, returns `Err(JsError::new("microphone permission denied"))` and does not spawn anything.
- `voice_stop`: drops `VoiceRuntime` (cancels all tasks within one iteration), tears down per-peer worklets and GainNodes, releases the MediaStream.
- `voice_input`: arrives from the capture worklet (real audio); calls `runtime.send_pcm(pcm)`.
- Per-peer volume: see Phase 6.1 revision above. The Gleam UI calls `voice.ffi.mjs::setPeerVolume(peerHex, gain)` directly — no Rust hop.
- `voice_inject_pcm`: bypasses the capture worklet and calls `runtime.send_pcm` directly. Used by tests to inject deterministic synthetic PCM.
- `voice_install_frame_recorder`: wraps the `FrameSink` in a recording adapter that captures `(peer, pcm)` pairs into an in-memory ring per peer.
- `voice_recorded_frames`: returns `[{seq_in_frame: number, len: 960, checksum: hex_string}, ...]` for the given peer. `seq_in_frame` is the embedded counter from the first sample (see test fixtures). `checksum` is a hash of the PCM bytes for byte-equal verification.
- `voice_active_peers`: returns the runtime's current per-peer `VoicePeerState` snapshot. Used by tests to assert FSM state without polling JS callbacks.

## Per-peer browser audio graph

```
on_frame from FrameSink ──► postMessage to playback-worklet[peer]
                                   │
                                   v
                            AudioWorkletNode[peer]
                                   │
                                   v
                              GainNode[peer]   ← voice_set_peer_volume
                                   │
                                   v
                           AudioContext.destination
                                   │
                                   v
                                speakers
                            (browser sums all peers automatically)
```

- One `AudioWorkletNode` (`voice-playback`) and one `GainNode` per peer, allocated lazily on the first `FrameSink::deliver` for that peer.
- On `FrameSink::drop_peer` (membership_liveness Stale), the per-peer chain is torn down: `disconnect()` the worklet and gain node, drop references.
- Mute-for-me on a peer = `voice_set_peer_volume(peer, 0.0)`. The audio still flows through the chain (so the recorder still sees it for tests), but the user hears nothing. UI restore-volume restores the prior value.

## Gleam UI wiring

### Model additions

```gleam
type VoiceModel {
  VoiceModel(
    self_in_call: Option(RoomId),  // None = not in call; Some = voice_start active for this room
    self_muted: Bool,
    self_deafened: Bool,
    peers: Dict(PeerHex, VoicePeerStateUI),
  )
}

type VoicePeerStateUI {
  VoicePeerStateUI(in_call: Bool, talking: Bool, is_muted: Bool)
}
```

`peers` is updated from the `on_voice_peer_state` FFI callback. Existing `voice_settings` (per-peer volume, denoise toggle, deafen-for-me) stays — it's read by the popover and on volume change writes through `voice_set_peer_volume`.

### Channel rail behaviour

The fixture today shows multiple voice channels per room (e.g. "Lounge"). For C2c we render exactly one voice channel per active room. Its name is the room name (or a fixed label "voice" — implementation pick; not contract). Other voice channels in the fixture are dropped.

The voice channel row:
- Idle (nobody in call): clickable; clicking calls `voice_start(room)`.
- Live (≥1 in call): expanded with member rail showing in-call members; click on the channel header toggles join/leave for the local user.

### Mute / deafen / leave wiring

| UI element | Existing? | Wires to |
|---|---|---|
| Mic icon on `voice_minibar` and `self_control_bar` | yes | `Client.voice_set_muted(!self.self_muted)`, model `self_muted` flips; visible state from model |
| Headphones icon on `voice_minibar` and `self_control_bar` | yes | `Client.voice_set_deafened(!self.self_deafened)`, model `self_deafened` flips |
| Leave icon on `voice_minibar` | yes | `Client.voice_stop()`, `self_in_call = None`; releases mic |
| Per-peer volume slider in `voice_popover` | yes | `Client.voice_set_peer_volume(peer_id, gain / 100.0)` on input |
| Mute-for-me toggle in `voice_popover` footer | yes | When on: `voice_set_peer_volume(peer_id, 0.0)` and remember prior gain. When off: restore. |
| Reset button in `voice_popover` footer | yes | Reset volume to 100% (`voice_set_peer_volume(peer_id, 1.0)`); UI denoise back on; mute-for-me off |
| Per-peer denoise toggle in `voice_popover` | yes | **Cosmetic in C2c.** UI state flips for visual feedback; no FFI wired. Real DSP wiring deferred. |

### Member-row indicators

Each member row in the voice-channel detail and the popover:
- `talking` from `peers[peer].talking` → existing pulsing speaker icon / animated ring.
- `is_muted` from `peers[peer].is_muted` → existing muted-mic badge (already in design assets).
- Self row: shows `self_muted` / `self_deafened` from model.

### Mic permission UX

`voice_start` is a Promise (the FFI signature returns `Result<(), JsError>` — JS sees a thrown error or a resolved Promise). On rejection with an error matching `/microphone/i`:

- Roll back model: `self_in_call = None`.
- Show a toast: "Microphone access required to join voice."
- Existing toast UI is reused (or a small one is added if none exists — flag during implementation).

## Test plan

### 1. Rust tests (`crates/sunset-voice`)

Fast, deterministic, run on every push. Cover `VoiceRuntime` in isolation with mocked `Dialer`, in-memory `Bus`, and recording `FrameSink`/`PeerStateSink`:

- Heartbeat publishes at the configured cadence; heartbeats carry the current `is_muted` state.
- Subscribe loop decrypts a packet from a different identity, dispatches to jitter buffer.
- Jitter buffer: pump emits at 20 ms cadence; overflow drops oldest; underrun produces one repeated frame then silence.
- Auto-connect FSM: first heartbeat from peer X triggers exactly one `Dialer::ensure_direct(X)`; repeated heartbeats are not re-dials; after `Gone` the next heartbeat re-fires `ensure_direct`.
- Mute: `set_muted(true)` causes `send_pcm` to drop frames (no `Bus::publish` call); heartbeat now carries `is_muted: true`; emitted `VoicePeerState` for self reflects it.
- Deafen: `set_deafened(true)` makes the jitter pump skip `FrameSink::deliver` but the combiner still emits accurate `talking` events.
- Drop semantics: dropping `VoiceRuntime` causes all five tasks to exit within one iteration.

### 2. Protocol regression Playwright test (kept harness)

`web/e2e/voice_protocol.spec.js` (renamed from current `voice_network.spec.js`, slimmed):

- Two browser contexts, both load `voice-e2e-test.html`.
- Both call `start({...})` and `startVoice()`. Auto-connect now happens inside `voice_start` so no manual `connectDirect` is needed.
- Alice calls `voice_inject_pcm` with a known synthetic frame.
- Bob's frame recorder asserts byte-equal received PCM within 5 s.
- Guards encryption + transport + codec passthrough.

The harness page is updated alongside the FFI: `voice_start` no longer takes an `on_frame` callback (frames go through `FrameSink` to per-peer worklets, never to JS user code). The harness installs the frame recorder via `voice_install_frame_recorder()` and queries with `voice_recorded_frames(peer_id)` instead of accumulating frames in an `on_frame` handler.

### 3. Real Gleam UI Playwright tests

All tests spawn a real `sunset-relay` per file (existing `beforeAll` pattern). Each test uses fresh identities (random seed) per peer.

#### Test fixtures

- **Frame injection** (`voice_inject_pcm`): synthetic PCM with an embedded counter. The first sample of each frame encodes a per-peer monotonically-increasing counter (scaled to the f32 range). Each peer's counter starts at 1.
- **Frame recording** (`voice_install_frame_recorder` + `voice_recorded_frames`): receiver records every `(peer_id, pcm)` the FrameSink would deliver. Returns `[{seq_in_frame, len, checksum}, ...]`.
- **One real-mic test**: launches Chromium with `--use-fake-device-for-media-stream --use-file-for-fake-audio-capture=web/audio/test-fixtures/sweep.wav` and exercises the actual capture worklet path end-to-end. Asserts only frame count + talking light, not content.

#### `voice_two_way.spec.js` — alice + bob via real chat UI

- Both load `/#room`, presence converges (member rail shows the other peer).
- Alice clicks the voice channel; `voice-minibar` appears within **500 ms** (Q5).
- Bob clicks the voice channel; both UIs show "2 in call" within **2 s**.
- Alice `voice_inject_pcm` 50 frames over 1 s with embedded counter.
- Bob's frame recorder must show within **3 s**:
  - ≥ 40 frames from alice's peer_id (allows 20% jitter-buffer drop).
  - Counter sequence is monotonically increasing.
  - No stretch of identical counter values longer than 5 frames (catches stuck-frame).
  - Every recorded frame's checksum matches the expected checksum for its counter (catches empty / wrong-frame / cross-peer mixup).
- Alice's UI shows bob's "talking" light go on within **200 ms** after bob's first injected frame.
- Real-mic test (separate `test()` block, fake-audio-capture flag): asserts ≥ 40 frames received and talking light flips within **3 s**.

#### `voice_three_way.spec.js`

- A + B + C all join. Each UI shows "3 in call" within **4 s** of the third joining (Q5).
- Alice injects 50 frames; both bob and carol's frame recorders pass the 2-way content checks for alice.
- Carol injects 50 frames; alice and bob both pass.
- Catches the bug where audio is delivered to one peer but not another (broken FrameSink dispatch or auto-connect missing one peer).

#### `voice_churn.spec.js`

- **Late joiner.** A + B in call, exchanging frames. C joins. Assert C's frame recorder receives frames from A within **3 s** of C joining. Assert A's UI shows C in_call within **2 s**.
- **Early leaver.** A + B + C in call. C calls `voice_stop` (clicks leave on minibar). A and B's UIs show C as not in_call within **6 s**. A injects after C leaves; B still receives normally.
- **Hard departure.** A + B + C in call. C closes the page (no `voice_stop`). A and B see C drop out within **6 s** (membership_liveness expiry). Assert via `voice_active_peers()` that C is no longer in A's runtime.
- **Re-join.** A + B in call. B leaves and re-joins (same identity — the page is not reloaded; only `voice_stop` then `voice_start`). The synthetic-PCM counter resets per `voice_start` call (it's per-injection-stream state, not per-identity). A's frame recorder for B shows two distinct epochs of monotonically-increasing counters with a temporal gap and a counter reset between them.

#### `voice_mute_deafen.spec.js`

- Alice clicks mic icon (mute on). Bob's UI shows muted-mic badge on alice within **2 s**. Bob's frame recorder shows alice's frame stream stops. Alice's heartbeats continue (assert via `voice_active_peers()` on bob: alice's `is_muted` is true).
- Alice clicks mic icon again (mute off). Frames flow again within **2 s**.
- Alice clicks headphones icon (deafen on). Alice's frame recorder stops growing for all peers (checksum-based: count freezes). Alice's UI still shows bob's `talking` light (subscribe still runs).
- Alice clicks headphones icon again (deafen off). Alice's frame recorder resumes within one jitter pump cycle (≤ **200 ms**).
- Per-peer mute-for-me: alice opens bob's voice popover, toggles mute-for-me. The DOM `GainNode` for bob is set to 0 (assert via WebAudio inspection). Alice's recorder still records (it captures pre-gain, by design).

#### `voice_mic_permission.spec.js`

- `context.clearPermissions()`; do not grant `microphone`.
- Click the voice channel. Assert no minibar appears, and an error/toast matching `/microphone/i` is visible within **2 s**.
- `context.grantPermissions(['microphone'])`. Click again. Minibar appears.

### Determinism / flake discipline

Per CLAUDE.md debugging discipline:

- **No `page.waitForFunction` polls of engine-internal state.** Tests poll user-visible UI state and the frame recorder (which is a user-equivalent: it captures exactly what the playback worklet would play).
- **No test-only synchronization signals.** Auto-connect runs inside `voice_start` exactly as it would for a real user. If a 4 s timeout for "third peer fully connected" trips, that's a real bug, not flake — the engine is too slow.
- **Deterministic synthetic PCM with embedded counters.** Any wrong-content bug — silent, repeated, swapped peer — fails an assertion, never lands in a "looks fine" state.
- **Fresh identity per test run** (existing pattern from `voice_network.spec.js`). Relay-side CRDT state from a previous run does not pollute.
- **Each multi-page test in its own `test()` block with separate `browser.newContext()`** (existing pattern).

### Existing tests that need updating

- `web/e2e/voice.spec.js` — currently tests the voice popover (volume slider, denoise toggle, deafen footer) against fixture data. With C2c the popover is wired to real state; the existing tests still exercise the popover UI but now need to be reframed against a real voice session (or kept fixture-only as a "popover renders correctly given a state shape" smoke test). Decision deferred to implementation: keep as a styling/interaction smoke or rewrite into the new real-UI suite.

### Things explicitly *not* tested in C2c

- Real opus encode/decode quality (codec is passthrough; byte-equal verification suffices).
- Real-DSP per-peer denoise (not implemented).
- Cross-browser (Firefox / Safari) Playwright runs.
- Network degradation (packet loss, bandwidth caps) beyond what unreliable WebRTC naturally exposes.
- Voice-channel-as-distinct-from-room (one voice channel per room in v1).

## Timing budgets (load-bearing)

| Event | Budget | Source |
|---|---|---|
| Click join → `voice-minibar` visible | < 500 ms | Q5 |
| Alice joins → bob's UI shows alice in_call | < 2 s | Q5 |
| Both in_call → first frame audible at peer | < 3 s | Q5 |
| Talking light on (after first frame arrives) | < 200 ms | Q5 |
| Talking light off (after speech stops) | < 1.2 s | Q5 (frame_liveness 1000 ms + jitter) |
| Leaving voice → in_call light off on peer | < 6 s | Q5 (membership_liveness 5000 ms + cadence). 10 s is the upper bound where users assume things are broken; 6 s clears that. |
| 3-way: third joiner fully connected | < 4 s | Q5 |
| Mute toggle → muted icon on peer | < 2 s | Q5 (one heartbeat at 2 s cadence) |

These budgets are encoded as hard-fail timeouts in the corresponding Playwright tests. A user-observable regression that pushes past any of them fails CI.

## Open questions / implementation flags

- **`is_muted` initial value.** When a peer first appears, before the first heartbeat lands, default `is_muted = false`. The combiner's debounce will emit the real value on the first heartbeat (~ at most 2 s). This is fine; UI just shows "unmuted" briefly which matches reality (they joined and we haven't heard otherwise).
- **Counter-encoded synthetic PCM range.** Picking the encoding (e.g. `sample[0] = (counter as f32) / 1e9`) is implementation detail; it must survive a round-trip through the passthrough codec exactly (it does, since passthrough is bit-identical).
- **Where the toast UI lives in the Gleam shell.** If no toast component exists, add a minimal one. Flag during implementation.
- **Per-peer denoise toggle storage.** Stays in `voice_settings`; no FFI wiring; revisit when DSP lands.
