# sunset-voice C2c Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Lift voice protocol logic from `sunset-web-wasm` into a host-agnostic `VoiceRuntime` in `sunset-voice`; wire join/leave + mute + deafen + per-peer volume to the real Gleam UI; auto-connect the WebRTC mesh on join; per-peer playback worklets with browser-side mixing; honest e2e Playwright suite covering 2-way / 3-way / churn / mute-deafen / mic permission with content-checked frame recording.

**Architecture:** `sunset-voice` grows a `VoiceRuntime` that owns heartbeats / subscribe / decrypt / liveness combiner / auto-connect / per-peer jitter buffer / mute & deafen state, parameterized over `Dialer`/`FrameSink`/`PeerStateSink` traits. `sunset-web-wasm/voice/` shrinks to a thin browser shell (audio worklets + GainNode + JS callbacks + trait impls). Gleam UI gets a `VoiceModel`, FFI bindings, and event handlers wired to the existing minibar/popover/channel-rail components.

**Tech Stack:** Rust (workspace, `?Send` single-threaded, `wasm32-unknown-unknown`), Gleam (Lustre), Playwright (Chromium), WebAudio (AudioWorklet, GainNode), WebRTC (unreliable datachannel via `sunset-sync-webrtc-browser`), `tokio` sync primitives only.

**Spec:** `docs/superpowers/specs/2026-05-03-sunset-voice-c2c-design.md`

---

## File structure

### NEW in `sunset-voice`

- `crates/sunset-voice/src/runtime/mod.rs` — `VoiceRuntime`, `VoiceTasks`, public re-exports
- `crates/sunset-voice/src/runtime/state.rs` — `RuntimeInner` shared-state struct (all interior mutability lives here)
- `crates/sunset-voice/src/runtime/traits.rs` — `Dialer`, `FrameSink`, `PeerStateSink`, `VoicePeerState`
- `crates/sunset-voice/src/runtime/heartbeat.rs` — heartbeat task future
- `crates/sunset-voice/src/runtime/subscribe.rs` — subscribe loop future
- `crates/sunset-voice/src/runtime/combiner.rs` — liveness combiner future
- `crates/sunset-voice/src/runtime/auto_connect.rs` — auto-connect FSM future
- `crates/sunset-voice/src/runtime/jitter.rs` — per-peer jitter buffer + pump future
- `crates/sunset-voice/tests/runtime_integration.rs` — integration tests using in-memory `Bus` + mock traits

### MODIFIED in `sunset-voice`

- `crates/sunset-voice/src/lib.rs` — `pub mod runtime;` + re-export `VoiceRuntime`, `VoiceTasks`, `VoicePeerState`, `Dialer`, `FrameSink`, `PeerStateSink`
- `crates/sunset-voice/src/packet.rs` — `Heartbeat` gains `is_muted: bool`; update frozen test vector
- `crates/sunset-voice/Cargo.toml` — add `async-trait`, `futures`, `tokio` (sync + macros + time + rt features), `tracing`, `web-time`, `wasmtimer` (cfg(target_arch="wasm32")), `pin-project`, dev-dep on `sunset-store-memory` + `sunset-noise` for in-memory Bus

### NEW in `sunset-web-wasm`

- `crates/sunset-web-wasm/src/voice/dialer.rs` — `WebDialer` impl wrapping `RoomHandle::connect_direct`
- `crates/sunset-web-wasm/src/voice/frame_sink.rs` — `WebFrameSink` impl that posts PCM to per-peer playback worklet via JS callback
- `crates/sunset-web-wasm/src/voice/peer_state_sink.rs` — `WebPeerStateSink` calls JS `on_voice_peer_state`
- `crates/sunset-web-wasm/src/voice/test_hooks.rs` — `RecorderFrameSink` wrapper + per-peer ring buffer (gated on `feature = "test-hooks"`)

### MODIFIED in `sunset-web-wasm`

- `crates/sunset-web-wasm/src/voice/mod.rs` — collapses to FFI shims that construct `VoiceRuntime` and assemble trait impls
- `crates/sunset-web-wasm/src/client.rs` — `voice_start` signature loses `on_frame`; new methods: `voice_set_muted`, `voice_set_deafened`, `voice_set_peer_volume`, plus test-hooks-gated `voice_inject_pcm`, `voice_install_frame_recorder`, `voice_recorded_frames`, `voice_active_peers`
- `crates/sunset-web-wasm/Cargo.toml` — add `feature = "test-hooks"`

### DELETED in `sunset-web-wasm`

- `crates/sunset-web-wasm/src/voice/transport.rs` — heartbeat moves to `sunset-voice/runtime/heartbeat.rs`
- `crates/sunset-web-wasm/src/voice/subscriber.rs` — moves to `sunset-voice/runtime/subscribe.rs`
- `crates/sunset-web-wasm/src/voice/liveness.rs` — moves to `sunset-voice/runtime/combiner.rs` + `state.rs`

### NEW in `web/`

- `web/src/sunset_web/voice.gleam` — Gleam side of the FFI bindings
- `web/src/sunset_web/voice.ffi.mjs` — JS FFI module: `getUserMedia`, capture worklet wiring, per-peer playback worklet table, GainNode table, FFI call surfaces
- `web/audio/test-fixtures/sweep.wav` — 5 s 440 Hz sine wave, mono 48 kHz, used by the real-mic test
- `web/audio/test-fixtures/README.md` — how the wav was generated
- `web/e2e/helpers/voice.js` — relay-spawn `beforeAll`, synthetic-PCM helpers, frame-recorder helpers, fresh-seed helper
- `web/e2e/voice_two_way.spec.js`
- `web/e2e/voice_three_way.spec.js`
- `web/e2e/voice_churn.spec.js`
- `web/e2e/voice_mute_deafen.spec.js`
- `web/e2e/voice_mic_permission.spec.js`
- `web/e2e/voice_real_mic.spec.js`

### MODIFIED in `web/`

- `web/voice-e2e-test.html` — uses frame recorder; drops `on_frame` callback parameter
- `web/e2e/voice_network.spec.js` → renamed `web/e2e/voice_protocol.spec.js`, slimmed
- `web/src/sunset_web.gleam` — `VoiceModel` field on the model; new `Msg` variants; `update`/`view` wiring; voice channel rendering switches from fixture to real data
- `web/src/sunset_web/views/channels.gleam` — voice channel row gets `on_click` to toggle join
- `web/src/sunset_web/views/voice_minibar.gleam` — buttons emit real `Msg` instead of UI-only state
- `web/src/sunset_web/views/voice_popover.gleam` — volume slider emits real `Msg`; mute-for-me wires to volume=0
- `web/src/sunset_web/fixture.gleam` — collapse to one voice channel per active room (or remove voice channels entirely if the UI now derives them from runtime state)
- `web/playwright.config.js` — `permissions: ['microphone']` for default project; new project for real-mic test with fake-media flag

---

## Phase 1 — `sunset-voice::runtime` (host-agnostic protocol)

### Task 1: Bump `Heartbeat` wire format with `is_muted`

**Files:**
- Modify: `crates/sunset-voice/src/packet.rs`

- [ ] **Step 1: Write the failing test** (in the existing `#[cfg(test)] mod tests` at the bottom of `packet.rs`):

```rust
#[test]
fn heartbeat_is_muted_round_trips() {
    let p = VoicePacket::Heartbeat { sent_at_ms: 12345, is_muted: true };
    let bytes = postcard::to_stdvec(&p).unwrap();
    let p2: VoicePacket = postcard::from_bytes(&bytes).unwrap();
    assert_eq!(p, p2);
    assert!(matches!(p2, VoicePacket::Heartbeat { is_muted: true, sent_at_ms: 12345 }));
}
```

- [ ] **Step 2: Run, expect failure**

```
nix develop --command cargo test -p sunset-voice --all-features heartbeat_is_muted_round_trips
```

Expected: compile error — `Heartbeat` has no `is_muted` field.

- [ ] **Step 3: Add the field**

In `crates/sunset-voice/src/packet.rs`, change:

```rust
pub enum VoicePacket {
    Frame { codec_id: String, seq: u64, sender_time_ms: u64, payload: Vec<u8> },
    Heartbeat { sent_at_ms: u64, is_muted: bool },
}
```

Update any other constructor of `Heartbeat` in this file (if a frozen test vector uses it, update the expected hex to match the new postcard encoding). Leave `Frame` unchanged.

- [ ] **Step 4: Update existing tests that construct `Heartbeat`**

Search:

```
grep -rn "VoicePacket::Heartbeat" crates/
```

Each call site in `crates/sunset-voice/` and `crates/sunset-web-wasm/` (the existing `transport.rs`) needs `is_muted: false` added. For `transport.rs` this is temporary — Phase 2 deletes the file.

- [ ] **Step 5: Run all sunset-voice tests, expect pass**

```
nix develop --command cargo test -p sunset-voice --all-features
```

Expected: PASS, including the existing `derive_voice_key_frozen_vector` (key derivation does not depend on `Heartbeat` shape) and the new `heartbeat_is_muted_round_trips`. Update any frozen postcard hex if needed.

- [ ] **Step 6: Commit**

```
git add crates/sunset-voice/src/packet.rs crates/sunset-web-wasm/src/voice/transport.rs
git commit -m "sunset-voice: Heartbeat carries is_muted

Wire-format change: VoicePacket::Heartbeat gains is_muted: bool.
Voice has not shipped, so no migration story — just bump the
encoding and update the call sites.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task 2: Define runtime traits and `VoicePeerState`

**Files:**
- Create: `crates/sunset-voice/src/runtime/mod.rs`
- Create: `crates/sunset-voice/src/runtime/traits.rs`
- Modify: `crates/sunset-voice/src/lib.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/sunset-voice/tests/runtime_traits.rs`:

```rust
use sunset_voice::runtime::{Dialer, FrameSink, PeerStateSink, VoicePeerState};
use sunset_sync::PeerId;
use std::cell::RefCell;
use std::rc::Rc;

struct RecordingDialer { calls: RefCell<Vec<PeerId>> }
#[async_trait::async_trait(?Send)]
impl Dialer for RecordingDialer {
    async fn ensure_direct(&self, peer: PeerId) {
        self.calls.borrow_mut().push(peer);
    }
}

struct RecordingFrameSink {
    delivered: RefCell<Vec<(PeerId, Vec<f32>)>>,
    dropped: RefCell<Vec<PeerId>>,
}
impl FrameSink for RecordingFrameSink {
    fn deliver(&self, peer: &PeerId, pcm: &[f32]) {
        self.delivered.borrow_mut().push((peer.clone(), pcm.to_vec()));
    }
    fn drop_peer(&self, peer: &PeerId) {
        self.dropped.borrow_mut().push(peer.clone());
    }
}

struct RecordingPeerStateSink { events: RefCell<Vec<VoicePeerState>> }
impl PeerStateSink for RecordingPeerStateSink {
    fn emit(&self, state: &VoicePeerState) {
        self.events.borrow_mut().push(state.clone());
    }
}

#[tokio::test(flavor = "current_thread")]
async fn traits_are_object_safe_and_implementable() {
    let d: Rc<dyn Dialer> = Rc::new(RecordingDialer { calls: RefCell::new(vec![]) });
    let f: Rc<dyn FrameSink> = Rc::new(RecordingFrameSink {
        delivered: RefCell::new(vec![]),
        dropped: RefCell::new(vec![]),
    });
    let p: Rc<dyn PeerStateSink> = Rc::new(RecordingPeerStateSink { events: RefCell::new(vec![]) });

    let dummy = PeerId(sunset_store::VerifyingKey::from_bytes([0u8; 32]));
    d.ensure_direct(dummy.clone()).await;
    f.deliver(&dummy, &[0.0_f32; 960]);
    f.drop_peer(&dummy);
    p.emit(&VoicePeerState { peer: dummy, in_call: true, talking: false, is_muted: false });
}
```

- [ ] **Step 2: Run, expect failure**

```
nix develop --command cargo test -p sunset-voice --all-features traits_are_object_safe
```

Expected: compile error — `sunset_voice::runtime` does not exist.

- [ ] **Step 3: Add `runtime` module to `lib.rs`**

```rust
pub mod runtime;
```

- [ ] **Step 4: Create `crates/sunset-voice/src/runtime/mod.rs`**

```rust
//! Host-agnostic voice runtime.
//!
//! `VoiceRuntime` owns the protocol state (heartbeat + subscribe +
//! liveness + auto-connect + jitter buffer + mute/deafen). Hosts
//! provide three traits: `Dialer` (ensure direct WebRTC connection),
//! `FrameSink` (deliver decoded PCM to the audio output), and
//! `PeerStateSink` (receive `VoicePeerState` change events).
//!
//! `?Send` throughout — single-threaded, matches the project's WASM
//! constraint. Hosts spawn the returned futures with whatever
//! single-threaded local-spawn primitive they have
//! (`wasm_bindgen_futures::spawn_local` for browser, `LocalSet::spawn_local`
//! for native).

mod traits;

pub use traits::{Dialer, FrameSink, PeerStateSink, VoicePeerState};
```

- [ ] **Step 5: Create `crates/sunset-voice/src/runtime/traits.rs`**

```rust
//! Host-supplied trait surface and the event type the runtime emits.

use async_trait::async_trait;

use sunset_sync::PeerId;

/// Per-peer voice state surfaced to the UI. The runtime emits a new
/// `VoicePeerState` whenever any of the three booleans changes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VoicePeerState {
    pub peer: PeerId,
    /// Heartbeats have arrived recently (or a frame, which implies
    /// the peer is in the call).
    pub in_call: bool,
    /// Frame heard within the last ~1 s.
    pub talking: bool,
    /// Last heartbeat reported `is_muted: true`. Default false until
    /// the first heartbeat lands.
    pub is_muted: bool,
}

/// Idempotent connection-establishment hook. The runtime calls this
/// when it sees a peer's heartbeat for the first time (or after the
/// peer was previously considered Gone). The host should ensure a
/// direct WebRTC connection exists. Repeat calls for an already-
/// connected peer must be cheap — the runtime does not deduplicate.
#[async_trait(?Send)]
pub trait Dialer {
    async fn ensure_direct(&self, peer: PeerId);
}

/// Sink for decoded PCM frames the runtime hands out at the jitter
/// pump cadence. PCM is `FRAME_SAMPLES` (960) f32 mono @ 48 kHz.
pub trait FrameSink {
    fn deliver(&self, peer: &PeerId, pcm: &[f32]);
    /// Peer transitioned from in-call to gone. Host should release
    /// per-peer playback resources (worklet node, gain node, etc.).
    fn drop_peer(&self, peer: &PeerId);
}

/// Sink for `VoicePeerState` change events. Called once per peer per
/// state transition (debounced — the runtime suppresses no-op repeats).
pub trait PeerStateSink {
    fn emit(&self, state: &VoicePeerState);
}
```

- [ ] **Step 6: Add deps to `crates/sunset-voice/Cargo.toml`**

```toml
[dependencies]
async-trait.workspace = true
# ... existing
sunset-sync.workspace = true   # needed for PeerId

[dev-dependencies]
# ... existing
tokio = { workspace = true, features = ["macros", "rt", "sync", "time"] }
async-trait.workspace = true
sunset-store = { path = "../sunset-store" }
```

(The `tokio` dev-dep is for `#[tokio::test]`.)

- [ ] **Step 7: Run, expect pass**

```
nix develop --command cargo test -p sunset-voice --all-features
```

Expected: PASS.

- [ ] **Step 8: Commit**

```
git commit -am "sunset-voice: Dialer/FrameSink/PeerStateSink trait surface"
```

---

### Task 3: `VoiceRuntime` skeleton + `RuntimeInner` shared state

**Files:**
- Create: `crates/sunset-voice/src/runtime/state.rs`
- Modify: `crates/sunset-voice/src/runtime/mod.rs`

- [ ] **Step 1: Write the failing test** (extend `tests/runtime_traits.rs` or add `tests/runtime_skeleton.rs`):

```rust
#[test]
fn voice_runtime_constructs_and_drops_cleanly() {
    use sunset_voice::runtime::VoiceRuntime;
    // Compile-test: VoiceRuntime exists. Body asserts trivially.
    let _ = std::any::TypeId::of::<VoiceRuntime>();
}
```

- [ ] **Step 2: Run, expect failure**

Expected: compile error — `VoiceRuntime` does not exist.

- [ ] **Step 3: Create `crates/sunset-voice/src/runtime/state.rs`**

```rust
//! Shared `RuntimeInner` — interior-mutable state every task references
//! through a `Weak`. Dropping the only `Rc<RuntimeInner>` (held by
//! `VoiceRuntime`) lets every task observe the upgrade failure and exit.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;
use std::sync::Arc;

use rand_chacha::ChaCha20Rng;
use sunset_core::Identity;
use sunset_core::Room;
use sunset_core::liveness::Liveness;
use sunset_sync::PeerId;

use crate::VoiceEncoder;
use crate::runtime::traits::{Dialer, FrameSink, PeerStateSink};

pub(crate) struct RuntimeInner {
    pub identity: Identity,
    pub room: Rc<Room>,
    pub bus: Arc<dyn DynBus>,
    pub dialer: Rc<dyn Dialer>,
    pub frame_sink: Rc<dyn FrameSink>,
    pub peer_state_sink: Rc<dyn PeerStateSink>,

    pub encoder: RefCell<VoiceEncoder>,
    pub seq: RefCell<u64>,
    pub rng: RefCell<ChaCha20Rng>,

    pub muted: RefCell<bool>,
    pub deafened: RefCell<bool>,

    pub frame_liveness: Arc<Liveness>,
    pub membership_liveness: Arc<Liveness>,

    /// Per-peer jitter buffers (`VecDeque<Vec<f32>>`). Used by the
    /// subscribe loop (push) and the jitter pump (pop). Per-peer
    /// auto-connect FSM state lives in a separate map.
    pub jitter: RefCell<HashMap<PeerId, VecDeque<Vec<f32>>>>,
    pub last_delivered: RefCell<HashMap<PeerId, Vec<f32>>>,
    pub auto_connect_state: RefCell<HashMap<PeerId, AutoConnectState>>,
    pub last_emitted: RefCell<HashMap<PeerId, EmittedState>>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum AutoConnectState {
    /// No heartbeat seen, or peer just transitioned Gone.
    Unknown,
    /// `dialer.ensure_direct` has been called; treat further heartbeats
    /// as no-op for dial purposes.
    Dialing,
}

/// Shape of the last `VoicePeerState` we emitted for a peer (for debounce).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct EmittedState {
    pub in_call: bool,
    pub talking: bool,
    pub is_muted: bool,
}

/// Type-erased `Bus`. The runtime takes an `Arc<dyn DynBus>` so it
/// doesn't need to be parameterized over `<S: Store, T: Transport>`.
/// Browsers + native pass an `Arc<BusImpl<...>>` cast to `dyn DynBus`.
#[async_trait::async_trait(?Send)]
pub trait DynBus {
    async fn publish_ephemeral(
        &self,
        name: bytes::Bytes,
        payload: bytes::Bytes,
    ) -> Result<(), Box<dyn std::error::Error>>;

    async fn subscribe_voice_prefix(
        &self,
        prefix: bytes::Bytes,
    ) -> Result<futures::stream::LocalBoxStream<'static, sunset_core::bus::BusEvent>, Box<dyn std::error::Error>>;
}
```

- [ ] **Step 4: Update `crates/sunset-voice/src/runtime/mod.rs`**

```rust
mod state;
mod traits;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use rand_chacha::ChaCha20Rng;
use rand_core::SeedableRng;

use sunset_core::Identity;
use sunset_core::Room;
use sunset_core::liveness::Liveness;

use crate::VoiceEncoder;

pub use state::DynBus;
pub use traits::{Dialer, FrameSink, PeerStateSink, VoicePeerState};

const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(2);
const FRAME_STALE_AFTER: Duration = Duration::from_millis(1000);
const MEMBERSHIP_STALE_AFTER: Duration = Duration::from_secs(5);
const JITTER_TARGET_DEPTH: usize = 4;
const JITTER_MAX_DEPTH: usize = 8;
const JITTER_PUMP_INTERVAL: Duration = Duration::from_millis(20);

pub struct VoiceRuntime {
    inner: Rc<state::RuntimeInner>,
}

pub struct VoiceTasks {
    pub heartbeat: futures::future::LocalBoxFuture<'static, ()>,
    pub subscribe: futures::future::LocalBoxFuture<'static, ()>,
    pub combiner: futures::future::LocalBoxFuture<'static, ()>,
    pub auto_connect: futures::future::LocalBoxFuture<'static, ()>,
    pub jitter_pump: futures::future::LocalBoxFuture<'static, ()>,
}

impl VoiceRuntime {
    pub fn new(
        bus: Arc<dyn DynBus>,
        room: Rc<Room>,
        identity: Identity,
        dialer: Rc<dyn Dialer>,
        frame_sink: Rc<dyn FrameSink>,
        peer_state_sink: Rc<dyn PeerStateSink>,
    ) -> (Self, VoiceTasks) {
        let now_nanos = web_time::SystemTime::now()
            .duration_since(web_time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);

        let frame_liveness = Liveness::new(FRAME_STALE_AFTER);
        let membership_liveness = Liveness::new(MEMBERSHIP_STALE_AFTER);

        let inner = Rc::new(state::RuntimeInner {
            identity,
            room,
            bus,
            dialer,
            frame_sink,
            peer_state_sink,
            encoder: RefCell::new(VoiceEncoder::new().expect("passthrough encoder construction is infallible")),
            seq: RefCell::new(0),
            rng: RefCell::new(ChaCha20Rng::seed_from_u64(now_nanos)),
            muted: RefCell::new(false),
            deafened: RefCell::new(false),
            frame_liveness,
            membership_liveness,
            jitter: RefCell::new(Default::default()),
            last_delivered: RefCell::new(Default::default()),
            auto_connect_state: RefCell::new(Default::default()),
            last_emitted: RefCell::new(Default::default()),
        });

        let tasks = VoiceTasks {
            heartbeat: heartbeat::spawn(Rc::downgrade(&inner)),
            subscribe: subscribe::spawn(Rc::downgrade(&inner)),
            combiner: combiner::spawn(Rc::downgrade(&inner)),
            auto_connect: auto_connect::spawn(Rc::downgrade(&inner)),
            jitter_pump: jitter::spawn(Rc::downgrade(&inner)),
        };

        (VoiceRuntime { inner }, tasks)
    }

    /// Capture-path entry. Encodes one frame, encrypts, publishes via
    /// `Bus::publish_ephemeral`. Drops the frame silently if `muted`.
    pub fn send_pcm(&self, _pcm: &[f32]) { /* implemented in Task 4 */ }

    pub fn set_muted(&self, muted: bool) {
        *self.inner.muted.borrow_mut() = muted;
    }

    pub fn set_deafened(&self, deafened: bool) {
        *self.inner.deafened.borrow_mut() = deafened;
    }

    /// Read mute state — used by heartbeat task and tests.
    #[doc(hidden)]
    pub fn is_muted(&self) -> bool {
        *self.inner.muted.borrow()
    }
}

mod auto_connect {
    use std::rc::Weak;
    use super::state::RuntimeInner;
    pub(crate) fn spawn(_inner: Weak<RuntimeInner>) -> futures::future::LocalBoxFuture<'static, ()> {
        Box::pin(async {})
    }
}
mod combiner {
    use std::rc::Weak;
    use super::state::RuntimeInner;
    pub(crate) fn spawn(_inner: Weak<RuntimeInner>) -> futures::future::LocalBoxFuture<'static, ()> {
        Box::pin(async {})
    }
}
mod heartbeat {
    use std::rc::Weak;
    use super::state::RuntimeInner;
    pub(crate) fn spawn(_inner: Weak<RuntimeInner>) -> futures::future::LocalBoxFuture<'static, ()> {
        Box::pin(async {})
    }
}
mod jitter {
    use std::rc::Weak;
    use super::state::RuntimeInner;
    pub(crate) fn spawn(_inner: Weak<RuntimeInner>) -> futures::future::LocalBoxFuture<'static, ()> {
        Box::pin(async {})
    }
}
mod subscribe {
    use std::rc::Weak;
    use super::state::RuntimeInner;
    pub(crate) fn spawn(_inner: Weak<RuntimeInner>) -> futures::future::LocalBoxFuture<'static, ()> {
        Box::pin(async {})
    }
}
```

(Inline empty stubs let the rest of the structure compile; tasks 4–8 will replace them with real files.)

- [ ] **Step 5: Run, expect pass**

```
nix develop --command cargo test -p sunset-voice --all-features voice_runtime_constructs
```

Expected: PASS.

- [ ] **Step 6: Commit**

```
git commit -am "sunset-voice: VoiceRuntime skeleton + RuntimeInner shared state"
```

---

### Task 4: Heartbeat task

**Files:**
- Create: `crates/sunset-voice/src/runtime/heartbeat.rs`
- Modify: `crates/sunset-voice/src/runtime/mod.rs` (replace inline stub with `mod heartbeat;`)
- Test: `crates/sunset-voice/tests/runtime_integration.rs`

- [ ] **Step 1: Add an in-memory `Bus` test fixture**

Create `crates/sunset-voice/tests/runtime_integration.rs` (we'll grow this file across tasks):

```rust
//! Integration tests for `VoiceRuntime` with an in-memory `Bus`.
//!
//! Uses tokio's `LocalSet` to spawn the runtime tasks alongside test
//! assertions. All `Bus` traffic loops back through a single
//! `BusImpl<MemoryStore, ...>`-equivalent test bus.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use futures::StreamExt;
use futures::stream::LocalBoxStream;
use tokio::sync::mpsc;

use sunset_core::Identity;
use sunset_core::Room;
use sunset_core::bus::BusEvent;
use sunset_sync::PeerId;
use sunset_voice::runtime::{
    Dialer, DynBus, FrameSink, PeerStateSink, VoicePeerState, VoiceRuntime,
};

/// Minimal in-memory `DynBus` for tests. Supports `publish_ephemeral`
/// and one `subscribe_voice_prefix` per test. Loopback is included
/// (publishes are visible to subscribers including the publisher).
struct TestBus {
    tx: tokio::sync::broadcast::Sender<sunset_core::bus::Datagram>,
    self_pk: sunset_store::VerifyingKey,
}

#[async_trait(?Send)]
impl DynBus for TestBus {
    async fn publish_ephemeral(
        &self,
        name: Bytes,
        payload: Bytes,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // Build a Datagram with self as verifying_key.
        let dgram = sunset_core::bus::Datagram {
            verifying_key: self.self_pk.clone(),
            name,
            payload,
        };
        let _ = self.tx.send(dgram);
        Ok(())
    }

    async fn subscribe_voice_prefix(
        &self,
        prefix: Bytes,
    ) -> Result<LocalBoxStream<'static, BusEvent>, Box<dyn std::error::Error>> {
        let mut rx = self.tx.subscribe();
        let stream = async_stream::stream! {
            loop {
                match rx.recv().await {
                    Ok(d) => {
                        if d.name.starts_with(&prefix) {
                            yield BusEvent::Ephemeral(d);
                        }
                    }
                    Err(_) => return,
                }
            }
        };
        Ok(Box::pin(stream))
    }
}

struct CountingDialer { calls: Rc<RefCell<Vec<PeerId>>> }
#[async_trait::async_trait(?Send)]
impl Dialer for CountingDialer {
    async fn ensure_direct(&self, peer: PeerId) { self.calls.borrow_mut().push(peer); }
}

struct RecordingFrameSink {
    delivered: Rc<RefCell<Vec<(PeerId, Vec<f32>)>>>,
    dropped: Rc<RefCell<Vec<PeerId>>>,
}
impl FrameSink for RecordingFrameSink {
    fn deliver(&self, peer: &PeerId, pcm: &[f32]) {
        self.delivered.borrow_mut().push((peer.clone(), pcm.to_vec()));
    }
    fn drop_peer(&self, peer: &PeerId) { self.dropped.borrow_mut().push(peer.clone()); }
}

struct RecordingPeerStateSink { events: Rc<RefCell<Vec<VoicePeerState>>> }
impl PeerStateSink for RecordingPeerStateSink {
    fn emit(&self, state: &VoicePeerState) { self.events.borrow_mut().push(state.clone()); }
}

fn make_identity_and_room(seed_byte: u8) -> (Identity, Rc<Room>) {
    let seed = [seed_byte; 32];
    let identity = sunset_core::Identity::from_seed(&seed).unwrap();
    let room = Rc::new(sunset_core::Room::open("test-room").unwrap());
    (identity, room)
}

#[tokio::test(flavor = "current_thread")]
async fn heartbeat_publishes_periodically_with_is_muted_flag() {
    tokio::task::LocalSet::new().run_until(async {
        let (alice, room) = make_identity_and_room(1);
        let pk = alice.store_verifying_key();
        let (tx, _) = tokio::sync::broadcast::channel(64);
        let bus: Arc<dyn DynBus> = Arc::new(TestBus { tx: tx.clone(), self_pk: pk.clone() });

        let dialer_calls: Rc<RefCell<Vec<PeerId>>> = Rc::new(RefCell::new(vec![]));
        let dialer: Rc<dyn Dialer> = Rc::new(CountingDialer { calls: dialer_calls });
        let frame_sink: Rc<dyn FrameSink> = Rc::new(RecordingFrameSink {
            delivered: Rc::new(RefCell::new(vec![])),
            dropped: Rc::new(RefCell::new(vec![])),
        });
        let peer_state_sink: Rc<dyn PeerStateSink> = Rc::new(RecordingPeerStateSink {
            events: Rc::new(RefCell::new(vec![])),
        });

        let (runtime, tasks) = VoiceRuntime::new(
            bus,
            room.clone(),
            alice.clone(),
            dialer,
            frame_sink,
            peer_state_sink,
        );
        tokio::task::spawn_local(tasks.heartbeat);

        // Subscribe ahead of the first heartbeat.
        let mut rx = tx.subscribe();

        // Initial heartbeat fires within ~10 ms, then every 2 s.
        // Speed-test: collect one heartbeat under a 3 s timeout.
        let hb = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let d = rx.recv().await.unwrap();
                if d.name.starts_with(b"voice/") { return d; }
            }
        }).await.expect("first heartbeat within 3s");

        // Decode and verify is_muted == false (default).
        let ev: sunset_voice::packet::EncryptedVoicePacket =
            postcard::from_bytes(&hb.payload).unwrap();
        let pkt = sunset_voice::packet::decrypt(&room, 0, &alice.public(), &ev).unwrap();
        match pkt {
            sunset_voice::packet::VoicePacket::Heartbeat { is_muted, .. } => {
                assert!(!is_muted, "default is_muted should be false");
            }
            _ => panic!("expected Heartbeat"),
        }

        // Toggle mute and capture another heartbeat.
        runtime.set_muted(true);
        let hb2 = tokio::time::timeout(Duration::from_secs(4), async {
            loop {
                let d = rx.recv().await.unwrap();
                if d.name.starts_with(b"voice/") { return d; }
            }
        }).await.expect("second heartbeat within 4s");
        let ev2: sunset_voice::packet::EncryptedVoicePacket =
            postcard::from_bytes(&hb2.payload).unwrap();
        let pkt2 = sunset_voice::packet::decrypt(&room, 0, &alice.public(), &ev2).unwrap();
        match pkt2 {
            sunset_voice::packet::VoicePacket::Heartbeat { is_muted, .. } => assert!(is_muted),
            _ => panic!("expected Heartbeat"),
        }

        drop(runtime); // task should exit
    }).await;
}
```

- [ ] **Step 2: Run, expect failure**

```
nix develop --command cargo test -p sunset-voice --all-features --test runtime_integration heartbeat_publishes
```

Expected: FAIL — heartbeat task is a no-op stub (no datagram is published).

- [ ] **Step 3: Implement `heartbeat.rs`**

Replace the inline `mod heartbeat` in `runtime/mod.rs` with `mod heartbeat;` and create `crates/sunset-voice/src/runtime/heartbeat.rs`:

```rust
//! Periodic heartbeat publisher. 2 s cadence, carries the runtime's
//! current `muted` flag.

use std::rc::Weak;

use bytes::Bytes;
use futures::FutureExt;
use rand_chacha::ChaCha20Rng;
use rand_core::SeedableRng;

use crate::packet::{VoicePacket, encrypt};
use super::{HEARTBEAT_INTERVAL, state::RuntimeInner};

pub(crate) fn spawn(weak: Weak<RuntimeInner>) -> futures::future::LocalBoxFuture<'static, ()> {
    async move {
        // Each task uses a divergent RNG seed so heartbeat nonces don't
        // collide with frame nonces.
        let now_nanos = web_time::SystemTime::now()
            .duration_since(web_time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        let mut rng = ChaCha20Rng::seed_from_u64(now_nanos ^ 0x55AA_55AA_55AA_55AA);

        loop {
            let Some(inner) = weak.upgrade() else { return; };
            let now_ms = web_time::SystemTime::now()
                .duration_since(web_time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            let muted = *inner.muted.borrow();
            let pkt = VoicePacket::Heartbeat { sent_at_ms: now_ms, is_muted: muted };
            let public = inner.identity.public();
            let room = inner.room.clone();
            let bus = inner.bus.clone();
            let room_fp = room.fingerprint().to_hex();
            let sender_pk = hex::encode(inner.identity.store_verifying_key().as_bytes());
            let name = Bytes::from(format!("voice/{room_fp}/{sender_pk}"));

            // Drop strong ref before awaiting so Drop can cancel us.
            drop(inner);

            match encrypt(&room, 0, &public, &pkt, &mut rng) {
                Ok(ev) => match postcard::to_stdvec(&ev) {
                    Ok(payload) => {
                        let _ = bus.publish_ephemeral(name, Bytes::from(payload)).await;
                    }
                    Err(e) => tracing::warn!(error = %e, "heartbeat postcard encode failed"),
                },
                Err(e) => tracing::warn!(error = %e, "heartbeat encrypt failed"),
            }

            sleep(HEARTBEAT_INTERVAL).await;
        }
    }
    .boxed_local()
}

#[cfg(target_arch = "wasm32")]
async fn sleep(d: std::time::Duration) { wasmtimer::tokio::sleep(d).await; }
#[cfg(not(target_arch = "wasm32"))]
async fn sleep(d: std::time::Duration) { tokio::time::sleep(d).await; }
```

Update `runtime/mod.rs`: remove the inline `mod heartbeat { ... }` stub and add `mod heartbeat;` near the other module declarations. Also add to `Cargo.toml`:

```toml
hex.workspace = true
web-time.workspace = true
futures.workspace = true
async-stream = "0.3"  # used by tests
[target.'cfg(target_arch = "wasm32")'.dependencies]
wasmtimer = "0.4"
```

- [ ] **Step 4: Run, expect pass**

```
nix develop --command cargo test -p sunset-voice --all-features --test runtime_integration heartbeat_publishes
```

Expected: PASS.

- [ ] **Step 5: Commit**

```
git commit -am "sunset-voice: heartbeat task carries is_muted, exits on Drop"
```

---

### Task 5: `send_pcm` (frame publish, mute-gated)

**Files:**
- Modify: `crates/sunset-voice/src/runtime/mod.rs` (implement `send_pcm`)
- Test: `crates/sunset-voice/tests/runtime_integration.rs`

- [ ] **Step 1: Write the failing test** (append to `runtime_integration.rs`):

```rust
#[tokio::test(flavor = "current_thread")]
async fn send_pcm_publishes_frame_when_unmuted() {
    tokio::task::LocalSet::new().run_until(async {
        let (alice, room) = make_identity_and_room(2);
        let pk = alice.store_verifying_key();
        let (tx, _) = tokio::sync::broadcast::channel(64);
        let bus: Arc<dyn DynBus> = Arc::new(TestBus { tx: tx.clone(), self_pk: pk.clone() });
        let dialer: Rc<dyn Dialer> = Rc::new(CountingDialer { calls: Rc::new(RefCell::new(vec![])) });
        let frame_sink: Rc<dyn FrameSink> = Rc::new(RecordingFrameSink {
            delivered: Rc::new(RefCell::new(vec![])),
            dropped: Rc::new(RefCell::new(vec![])),
        });
        let peer_state_sink: Rc<dyn PeerStateSink> = Rc::new(RecordingPeerStateSink {
            events: Rc::new(RefCell::new(vec![])),
        });

        let (runtime, _tasks) = VoiceRuntime::new(bus, room.clone(), alice.clone(), dialer, frame_sink, peer_state_sink);
        let mut rx = tx.subscribe();

        let pcm: Vec<f32> = (0..960).map(|i| (i as f32) / 1000.0).collect();
        runtime.send_pcm(&pcm);

        let frame = tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let d = rx.recv().await.unwrap();
                if d.name.starts_with(b"voice/") { return d; }
            }
        }).await.expect("frame within 1s");

        let ev: sunset_voice::packet::EncryptedVoicePacket = postcard::from_bytes(&frame.payload).unwrap();
        let pkt = sunset_voice::packet::decrypt(&room, 0, &alice.public(), &ev).unwrap();
        let bytes = match pkt {
            sunset_voice::packet::VoicePacket::Frame { payload, .. } => payload,
            _ => panic!("expected Frame"),
        };
        let mut decoder = sunset_voice::VoiceDecoder::new().unwrap();
        let decoded = decoder.decode(&bytes).unwrap();
        assert_eq!(decoded, pcm);
    }).await;
}

#[tokio::test(flavor = "current_thread")]
async fn send_pcm_drops_frames_when_muted() {
    tokio::task::LocalSet::new().run_until(async {
        let (alice, room) = make_identity_and_room(3);
        let pk = alice.store_verifying_key();
        let (tx, _) = tokio::sync::broadcast::channel(64);
        let bus: Arc<dyn DynBus> = Arc::new(TestBus { tx: tx.clone(), self_pk: pk.clone() });
        let dialer: Rc<dyn Dialer> = Rc::new(CountingDialer { calls: Rc::new(RefCell::new(vec![])) });
        let frame_sink: Rc<dyn FrameSink> = Rc::new(RecordingFrameSink {
            delivered: Rc::new(RefCell::new(vec![])),
            dropped: Rc::new(RefCell::new(vec![])),
        });
        let peer_state_sink: Rc<dyn PeerStateSink> = Rc::new(RecordingPeerStateSink {
            events: Rc::new(RefCell::new(vec![])),
        });
        let (runtime, _tasks) = VoiceRuntime::new(bus, room.clone(), alice.clone(), dialer, frame_sink, peer_state_sink);
        runtime.set_muted(true);

        let mut rx = tx.subscribe();
        let pcm = vec![0.1_f32; 960];
        runtime.send_pcm(&pcm);

        // Wait briefly: no frame packet should arrive (heartbeats may).
        let r = tokio::time::timeout(Duration::from_millis(300), async {
            loop {
                let d = rx.recv().await.unwrap();
                if d.name.starts_with(b"voice/") {
                    // Decrypt and check whether it's a Frame.
                    let ev: sunset_voice::packet::EncryptedVoicePacket = postcard::from_bytes(&d.payload).unwrap();
                    let pkt = sunset_voice::packet::decrypt(&room, 0, &alice.public(), &ev).unwrap();
                    if matches!(pkt, sunset_voice::packet::VoicePacket::Frame { .. }) { return d; }
                }
            }
        }).await;
        assert!(r.is_err(), "no Frame should be published while muted");
    }).await;
}
```

- [ ] **Step 2: Run, expect failure**

```
nix develop --command cargo test -p sunset-voice --all-features --test runtime_integration send_pcm
```

Expected: FAIL — `send_pcm` is a no-op.

- [ ] **Step 3: Implement `send_pcm`**

In `crates/sunset-voice/src/runtime/mod.rs`, replace the stub:

```rust
impl VoiceRuntime {
    pub fn send_pcm(&self, pcm: &[f32]) {
        if *self.inner.muted.borrow() { return; }
        if pcm.len() != crate::FRAME_SAMPLES { return; }

        let inner = self.inner.clone();
        let pcm = pcm.to_vec();
        // Spawn the publish — Bus::publish_ephemeral is async. We
        // can't .await synchronously here.
        spawn_local(async move {
            let encoded = match inner.encoder.borrow_mut().encode(&pcm) {
                Ok(b) => b,
                Err(e) => { tracing::warn!(error = %e, "encode failed"); return; }
            };
            let now_ms = web_time::SystemTime::now()
                .duration_since(web_time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            let seq = { let mut s = inner.seq.borrow_mut(); let v = *s; *s = s.saturating_add(1); v };
            let pkt = crate::packet::VoicePacket::Frame {
                codec_id: crate::CODEC_ID.to_string(),
                seq,
                sender_time_ms: now_ms,
                payload: encoded,
            };
            let public = inner.identity.public();
            let ev = match crate::packet::encrypt(&inner.room, 0, &public, &pkt, &mut *inner.rng.borrow_mut()) {
                Ok(e) => e,
                Err(e) => { tracing::warn!(error = %e, "encrypt failed"); return; }
            };
            let payload = match postcard::to_stdvec(&ev) {
                Ok(p) => p,
                Err(e) => { tracing::warn!(error = %e, "postcard encode failed"); return; }
            };
            let room_fp = inner.room.fingerprint().to_hex();
            let sender_pk = hex::encode(inner.identity.store_verifying_key().as_bytes());
            let name = bytes::Bytes::from(format!("voice/{room_fp}/{sender_pk}"));
            let _ = inner.bus.publish_ephemeral(name, bytes::Bytes::from(payload)).await;
        });
    }
}

#[cfg(target_arch = "wasm32")]
fn spawn_local<F: std::future::Future<Output = ()> + 'static>(f: F) { wasm_bindgen_futures::spawn_local(f); }
#[cfg(not(target_arch = "wasm32"))]
fn spawn_local<F: std::future::Future<Output = ()> + 'static>(f: F) { tokio::task::spawn_local(f); }
```

Add `[target.'cfg(target_arch = "wasm32")'.dependencies] wasm-bindgen-futures = "0.4"` to `crates/sunset-voice/Cargo.toml`.

- [ ] **Step 4: Run, expect pass**

```
nix develop --command cargo test -p sunset-voice --all-features --test runtime_integration send_pcm
```

Expected: PASS for both tests.

- [ ] **Step 5: Commit**

```
git commit -am "sunset-voice: send_pcm publishes Frame; muted drops frames"
```

---

### Task 6: Subscribe loop (decrypt, dispatch by enum, feed liveness + jitter)

**Files:**
- Create: `crates/sunset-voice/src/runtime/subscribe.rs`
- Modify: `crates/sunset-voice/src/runtime/mod.rs`
- Test: `crates/sunset-voice/tests/runtime_integration.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test(flavor = "current_thread")]
async fn subscribe_decrypts_frame_and_pushes_to_jitter() {
    tokio::task::LocalSet::new().run_until(async {
        let (alice, room) = make_identity_and_room(4);
        let (bob, _) = make_identity_and_room(5);
        let alice_pk = alice.store_verifying_key();

        let (tx, _) = tokio::sync::broadcast::channel(64);
        let bob_bus: Arc<dyn DynBus> = Arc::new(TestBus { tx: tx.clone(), self_pk: bob.store_verifying_key() });
        let dialer: Rc<dyn Dialer> = Rc::new(CountingDialer { calls: Rc::new(RefCell::new(vec![])) });
        let delivered: Rc<RefCell<Vec<(PeerId, Vec<f32>)>>> = Rc::new(RefCell::new(vec![]));
        let frame_sink: Rc<dyn FrameSink> = Rc::new(RecordingFrameSink {
            delivered: delivered.clone(),
            dropped: Rc::new(RefCell::new(vec![])),
        });
        let peer_state_sink: Rc<dyn PeerStateSink> = Rc::new(RecordingPeerStateSink {
            events: Rc::new(RefCell::new(vec![])),
        });

        let (_runtime, tasks) = VoiceRuntime::new(bob_bus, room.clone(), bob.clone(), dialer, frame_sink, peer_state_sink);
        tokio::task::spawn_local(tasks.subscribe);

        // Alice publishes one Frame as if she were on the network.
        let pcm: Vec<f32> = (0..960).map(|i| (i as f32) * 0.001).collect();
        let mut enc = sunset_voice::VoiceEncoder::new().unwrap();
        let bytes = enc.encode(&pcm).unwrap();
        let pkt = sunset_voice::packet::VoicePacket::Frame {
            codec_id: sunset_voice::CODEC_ID.to_string(),
            seq: 1,
            sender_time_ms: 1000,
            payload: bytes,
        };
        let mut rng = rand_chacha::ChaCha20Rng::seed_from_u64(42);
        let ev = sunset_voice::packet::encrypt(&room, 0, &alice.public(), &pkt, &mut rng).unwrap();
        let payload = postcard::to_stdvec(&ev).unwrap();
        let room_fp = room.fingerprint().to_hex();
        let sender_pk = hex::encode(alice_pk.as_bytes());
        let name = bytes::Bytes::from(format!("voice/{room_fp}/{sender_pk}"));

        // Inject as if it came through the bus from alice.
        let dgram = sunset_core::bus::Datagram { verifying_key: alice_pk.clone(), name, payload: bytes::Bytes::from(payload) };
        let _ = tx.send(dgram);

        // Wait for the subscribe loop to push the frame into the jitter buffer.
        // Verify by polling internal state via VoiceRuntime test helper, OR
        // via emitted talking event from the combiner. For now: check
        // FrameSink directly is wrong (jitter pump hasn't run).
        // Instead poll the delivered list AFTER spawning jitter pump too.
        // Re-do this test to spawn the jitter pump as well.
        // Actually for THIS task we only verify that subscribe ran and
        // reached the encrypted/decrypted state — assert via a test-only
        // accessor: `runtime.test_frame_observed_count(peer)`.
        // (Implementer: add a test-only accessor under #[cfg(test)] that
        //  reports the inner.jitter buffer length for `peer`.)
        let _ = delivered; // unused at this layer
    }).await;
}
```

(The task description in the failing-test step deliberately points the implementer to add a `#[cfg(test)]` accessor on `VoiceRuntime` that reports `inner.jitter[peer].len()` — keeping engine-internal probing inside `#[cfg(test)]` so production code never depends on it.)

- [ ] **Step 2: Run, expect failure**

```
nix develop --command cargo test -p sunset-voice --all-features --test runtime_integration subscribe_decrypts
```

Expected: FAIL or panic — subscribe task is no-op stub.

- [ ] **Step 3: Implement `subscribe.rs`**

Create `crates/sunset-voice/src/runtime/subscribe.rs`:

```rust
//! Subscribe loop: opens a Bus subscription with prefix `voice/<fp>/`,
//! decrypts each `EncryptedVoicePacket`, dispatches by enum:
//! - `Frame` → feed `frame_liveness` + push decoded PCM to per-peer
//!   jitter buffer.
//! - `Heartbeat` → feed `membership_liveness` + record `is_muted` so
//!   the combiner can emit it.

use std::rc::Weak;
use std::time::SystemTime;

use bytes::Bytes;
use futures::{FutureExt, StreamExt};

use sunset_core::bus::BusEvent;
use sunset_core::identity::IdentityKey;
use sunset_sync::PeerId;

use crate::packet::{EncryptedVoicePacket, VoicePacket, decrypt};
use super::state::RuntimeInner;

pub(crate) fn spawn(weak: Weak<RuntimeInner>) -> futures::future::LocalBoxFuture<'static, ()> {
    async move {
        let Some(inner) = weak.upgrade() else { return; };
        let room_fp = inner.room.fingerprint().to_hex();
        let prefix = Bytes::from(format!("voice/{room_fp}/"));
        let bus = inner.bus.clone();
        let self_pk = inner.identity.store_verifying_key();
        drop(inner);

        let mut stream = match bus.subscribe_voice_prefix(prefix).await {
            Ok(s) => s,
            Err(e) => { tracing::error!(error = %e, "subscribe failed"); return; }
        };
        let mut decoder = match crate::VoiceDecoder::new() {
            Ok(d) => d,
            Err(e) => { tracing::error!(error = %e, "decoder init failed"); return; }
        };

        while let Some(ev) = stream.next().await {
            let Some(inner) = weak.upgrade() else { return; };
            let datagram = match ev {
                BusEvent::Ephemeral(d) => d,
                BusEvent::Durable { .. } => continue,
            };
            if datagram.verifying_key == self_pk { continue; }
            let peer = PeerId(datagram.verifying_key.clone());
            let sender = match IdentityKey::from_store_verifying_key(&datagram.verifying_key) {
                Ok(s) => s,
                Err(_) => continue,
            };
            let evp: EncryptedVoicePacket = match postcard::from_bytes(&datagram.payload) {
                Ok(e) => e,
                Err(_) => continue,
            };
            let packet = match decrypt(&inner.room, 0, &sender, &evp) {
                Ok(p) => p,
                Err(e) => { tracing::warn!(error = %e, "decrypt failed"); continue; }
            };
            match packet {
                VoicePacket::Frame { payload, sender_time_ms, .. } => {
                    let st = SystemTime::UNIX_EPOCH + std::time::Duration::from_millis(sender_time_ms);
                    inner.frame_liveness.observe(peer.clone(), st).await;
                    match decoder.decode(&payload) {
                        Ok(pcm) => {
                            let mut jitter = inner.jitter.borrow_mut();
                            let q = jitter.entry(peer).or_default();
                            q.push_back(pcm);
                            // Cap at JITTER_MAX_DEPTH (configured in mod.rs).
                            while q.len() > super::JITTER_MAX_DEPTH {
                                q.pop_front();
                            }
                        }
                        Err(e) => tracing::warn!(error = %e, "decode failed"),
                    }
                }
                VoicePacket::Heartbeat { sent_at_ms, is_muted } => {
                    let st = SystemTime::UNIX_EPOCH + std::time::Duration::from_millis(sent_at_ms);
                    inner.membership_liveness.observe(peer.clone(), st).await;
                    inner.last_emitted_set_muted_seen(peer, is_muted);
                }
            }
        }
    }.boxed_local()
}
```

In `state.rs` add the `last_emitted_set_muted_seen` helper:

```rust
impl RuntimeInner {
    /// Record the `is_muted` flag from a heartbeat. The combiner reads
    /// this when emitting `VoicePeerState`.
    pub(crate) fn last_emitted_set_muted_seen(&self, peer: PeerId, is_muted: bool) {
        let mut map = self.last_emitted.borrow_mut();
        let entry = map.entry(peer).or_insert(EmittedState {
            in_call: false, talking: false, is_muted: false,
        });
        entry.is_muted = is_muted;
    }
}
```

(Note: this stores `is_muted` *as last seen* on the existing `last_emitted` map; the combiner's debounce logic will read it. Implementer may prefer a separate `last_seen_muted: HashMap` — refactor as tidy.)

Update `runtime/mod.rs`: replace `mod subscribe { ... }` stub with `mod subscribe;`.

- [ ] **Step 4: Add the test-only accessor and update the test to assert via it**

Add to `runtime/mod.rs` inside `impl VoiceRuntime`:

```rust
#[cfg(test)]
pub fn test_jitter_len(&self, peer: &sunset_sync::PeerId) -> usize {
    self.inner.jitter.borrow().get(peer).map(|q| q.len()).unwrap_or(0)
}
```

Update the test to poll `runtime.test_jitter_len(&PeerId(alice_pk))` until ≥ 1, with a 1 s timeout.

- [ ] **Step 5: Run, expect pass**

```
nix develop --command cargo test -p sunset-voice --all-features --test runtime_integration subscribe_decrypts
```

Expected: PASS.

- [ ] **Step 6: Commit**

```
git commit -am "sunset-voice: subscribe loop decrypts and pushes to per-peer jitter"
```

---

### Task 7: Liveness combiner emits `VoicePeerState`

**Files:**
- Create: `crates/sunset-voice/src/runtime/combiner.rs`
- Modify: `crates/sunset-voice/src/runtime/mod.rs`
- Test: `crates/sunset-voice/tests/runtime_integration.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test(flavor = "current_thread")]
async fn combiner_emits_state_on_heartbeat() {
    tokio::task::LocalSet::new().run_until(async {
        let (alice, room) = make_identity_and_room(6);
        let (bob, _) = make_identity_and_room(7);
        let (tx, _) = tokio::sync::broadcast::channel(64);
        let bob_bus: Arc<dyn DynBus> = Arc::new(TestBus { tx: tx.clone(), self_pk: bob.store_verifying_key() });
        let dialer: Rc<dyn Dialer> = Rc::new(CountingDialer { calls: Rc::new(RefCell::new(vec![])) });
        let frame_sink: Rc<dyn FrameSink> = Rc::new(RecordingFrameSink {
            delivered: Rc::new(RefCell::new(vec![])),
            dropped: Rc::new(RefCell::new(vec![])),
        });
        let events: Rc<RefCell<Vec<VoicePeerState>>> = Rc::new(RefCell::new(vec![]));
        let peer_state_sink: Rc<dyn PeerStateSink> = Rc::new(RecordingPeerStateSink { events: events.clone() });

        let (_runtime, tasks) = VoiceRuntime::new(bob_bus, room.clone(), bob.clone(), dialer, frame_sink, peer_state_sink);
        tokio::task::spawn_local(tasks.subscribe);
        tokio::task::spawn_local(tasks.combiner);

        // Inject one Heartbeat from alice with is_muted=true.
        let pkt = sunset_voice::packet::VoicePacket::Heartbeat { sent_at_ms: 5000, is_muted: true };
        let mut rng = rand_chacha::ChaCha20Rng::seed_from_u64(99);
        let ev = sunset_voice::packet::encrypt(&room, 0, &alice.public(), &pkt, &mut rng).unwrap();
        let payload = postcard::to_stdvec(&ev).unwrap();
        let room_fp = room.fingerprint().to_hex();
        let sender_pk = hex::encode(alice.store_verifying_key().as_bytes());
        let name = bytes::Bytes::from(format!("voice/{room_fp}/{sender_pk}"));
        let dgram = sunset_core::bus::Datagram { verifying_key: alice.store_verifying_key(), name, payload: bytes::Bytes::from(payload) };
        let _ = tx.send(dgram);

        // Wait for emitted state.
        let result = tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if let Some(ev) = events.borrow().last().cloned() { return ev; }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }).await.expect("emit within 1s");

        assert_eq!(result.peer, PeerId(alice.store_verifying_key()));
        assert!(result.in_call);
        assert!(!result.talking);
        assert!(result.is_muted);
    }).await;
}
```

- [ ] **Step 2: Run, expect failure**

Expected: FAIL — combiner is no-op stub; events list stays empty.

- [ ] **Step 3: Implement `combiner.rs`**

Create `crates/sunset-voice/src/runtime/combiner.rs`:

```rust
//! Combines the two `Liveness` streams into `VoicePeerState`. Debounces
//! by suppressing emissions when (in_call, talking, is_muted) doesn't
//! change for a peer.

use std::rc::Weak;

use futures::{FutureExt, StreamExt};

use sunset_core::liveness::LivenessState;

use super::state::{EmittedState, RuntimeInner};
use super::traits::VoicePeerState;

pub(crate) fn spawn(weak: Weak<RuntimeInner>) -> futures::future::LocalBoxFuture<'static, ()> {
    async move {
        let Some(inner) = weak.upgrade() else { return; };
        let frame_arc = inner.frame_liveness.clone();
        let membership_arc = inner.membership_liveness.clone();
        drop(inner);

        let mut frame_sub = frame_arc.subscribe().await;
        let mut membership_sub = membership_arc.subscribe().await;

        loop {
            tokio::select! {
                Some(ev) = frame_sub.next() => {
                    let Some(inner) = weak.upgrade() else { return; };
                    let alive = ev.state == LivenessState::Live;
                    let mut last = inner.last_emitted.borrow_mut();
                    let entry = last.entry(ev.peer.clone()).or_insert(EmittedState {
                        in_call: false, talking: false, is_muted: false,
                    });
                    let mut new = *entry;
                    new.talking = alive;
                    new.in_call = alive || new.in_call;  // talking implies in_call
                    if new != *entry {
                        *entry = new;
                        let state = VoicePeerState { peer: ev.peer.clone(), in_call: new.in_call, talking: new.talking, is_muted: new.is_muted };
                        let sink = inner.peer_state_sink.clone();
                        drop(last);
                        sink.emit(&state);
                    }
                }
                Some(ev) = membership_sub.next() => {
                    let Some(inner) = weak.upgrade() else { return; };
                    let alive = ev.state == LivenessState::Live;
                    let mut last = inner.last_emitted.borrow_mut();
                    let entry = last.entry(ev.peer.clone()).or_insert(EmittedState {
                        in_call: false, talking: false, is_muted: false,
                    });
                    let mut new = *entry;
                    // membership Live → in_call=true; Stale → in_call=talking
                    new.in_call = alive || new.talking;
                    if new != *entry {
                        *entry = new;
                        let state = VoicePeerState { peer: ev.peer.clone(), in_call: new.in_call, talking: new.talking, is_muted: new.is_muted };
                        let sink = inner.peer_state_sink.clone();
                        drop(last);
                        sink.emit(&state);
                    }
                }
                else => return,
            }
        }
    }.boxed_local()
}
```

Replace `mod combiner { ... }` stub in `mod.rs` with `mod combiner;`.

The subscriber's `last_emitted_set_muted_seen` records `is_muted` into the same `last_emitted` map; the next combiner emission picks it up. *Implementer may choose a tighter approach where heartbeat receipt itself triggers a combiner emission for `is_muted` changes.* For now, `is_muted` becomes visible on the next liveness tick (worst case ~one heartbeat).

- [ ] **Step 4: Add an explicit emit-on-mute-change path**

To make the mute-icon-within-2s budget tight, the subscribe loop should call into the combiner's emit logic directly when it observes a heartbeat with a different `is_muted` than the previously stored one. Add to `state.rs`:

```rust
impl RuntimeInner {
    /// Returns true if is_muted differs from previously stored.
    pub(crate) fn last_emitted_set_muted_seen(&self, peer: PeerId, is_muted: bool) -> bool {
        let mut map = self.last_emitted.borrow_mut();
        let entry = map.entry(peer).or_insert(EmittedState {
            in_call: false, talking: false, is_muted: false,
        });
        if entry.is_muted != is_muted {
            entry.is_muted = is_muted;
            true
        } else {
            false
        }
    }
}
```

In `subscribe.rs`, after `last_emitted_set_muted_seen` returns true, call:

```rust
if inner.last_emitted_set_muted_seen(peer.clone(), is_muted) {
    let entry = *inner.last_emitted.borrow().get(&peer).unwrap();
    let state = VoicePeerState {
        peer: peer.clone(),
        in_call: entry.in_call,
        talking: entry.talking,
        is_muted: entry.is_muted,
    };
    inner.peer_state_sink.emit(&state);
}
```

- [ ] **Step 5: Run, expect pass**

```
nix develop --command cargo test -p sunset-voice --all-features --test runtime_integration combiner_emits_state
```

Expected: PASS.

- [ ] **Step 6: Commit**

```
git commit -am "sunset-voice: combiner emits VoicePeerState including is_muted"
```

---

### Task 8: Auto-connect FSM

**Files:**
- Create: `crates/sunset-voice/src/runtime/auto_connect.rs`
- Modify: `crates/sunset-voice/src/runtime/mod.rs`
- Modify: `crates/sunset-voice/src/runtime/subscribe.rs` (notify auto-connect on heartbeat)
- Modify: `crates/sunset-voice/src/runtime/state.rs` (add channel for auto-connect notifications)
- Test: `crates/sunset-voice/tests/runtime_integration.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test(flavor = "current_thread")]
async fn auto_connect_dials_first_heartbeat_only_once() {
    tokio::task::LocalSet::new().run_until(async {
        let (alice, room) = make_identity_and_room(8);
        let (bob, _) = make_identity_and_room(9);
        let (tx, _) = tokio::sync::broadcast::channel(64);
        let bus: Arc<dyn DynBus> = Arc::new(TestBus { tx: tx.clone(), self_pk: bob.store_verifying_key() });
        let calls: Rc<RefCell<Vec<PeerId>>> = Rc::new(RefCell::new(vec![]));
        let dialer: Rc<dyn Dialer> = Rc::new(CountingDialer { calls: calls.clone() });
        let frame_sink: Rc<dyn FrameSink> = Rc::new(RecordingFrameSink {
            delivered: Rc::new(RefCell::new(vec![])),
            dropped: Rc::new(RefCell::new(vec![])),
        });
        let peer_state_sink: Rc<dyn PeerStateSink> = Rc::new(RecordingPeerStateSink {
            events: Rc::new(RefCell::new(vec![])),
        });
        let (_runtime, tasks) = VoiceRuntime::new(bus, room.clone(), bob.clone(), dialer, frame_sink, peer_state_sink);
        tokio::task::spawn_local(tasks.subscribe);
        tokio::task::spawn_local(tasks.auto_connect);

        // Three heartbeats from alice — only the first should trigger ensure_direct.
        for _ in 0..3 {
            let pkt = sunset_voice::packet::VoicePacket::Heartbeat { sent_at_ms: 1000, is_muted: false };
            let mut rng = rand_chacha::ChaCha20Rng::seed_from_u64(0xab);
            let ev = sunset_voice::packet::encrypt(&room, 0, &alice.public(), &pkt, &mut rng).unwrap();
            let payload = postcard::to_stdvec(&ev).unwrap();
            let room_fp = room.fingerprint().to_hex();
            let sender_pk = hex::encode(alice.store_verifying_key().as_bytes());
            let name = bytes::Bytes::from(format!("voice/{room_fp}/{sender_pk}"));
            let _ = tx.send(sunset_core::bus::Datagram {
                verifying_key: alice.store_verifying_key(),
                name, payload: bytes::Bytes::from(payload),
            });
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(calls.borrow().len(), 1, "ensure_direct must be called exactly once");
    }).await;
}
```

- [ ] **Step 2: Run, expect failure**

Expected: FAIL — auto_connect is no-op stub; calls is empty.

- [ ] **Step 3: Implement notification channel + FSM**

In `state.rs` add:

```rust
use tokio::sync::mpsc;
pub(crate) struct AutoConnectChan {
    pub tx: mpsc::UnboundedSender<PeerId>,
    pub rx: RefCell<Option<mpsc::UnboundedReceiver<PeerId>>>,  // taken once by auto_connect task
}

// add to RuntimeInner:
pub auto_connect_chan: AutoConnectChan,
```

Initialize in `RuntimeInner::new` (or in `VoiceRuntime::new`): `let (tx, rx) = mpsc::unbounded_channel(); ...`.

In `subscribe.rs`, after observing a heartbeat (whether or not is_muted changed), notify:

```rust
let _ = inner.auto_connect_chan.tx.send(peer.clone());
```

Create `crates/sunset-voice/src/runtime/auto_connect.rs`:

```rust
//! Auto-connect FSM: per-peer Unknown → Dialing → (eventually Gone via
//! membership_liveness Stale → back to Unknown).
//!
//! Notifications come from the subscribe loop on every heartbeat.
//! Liveness Stale events come from the membership_liveness subscribe.

use std::rc::Weak;

use futures::{FutureExt, StreamExt};

use sunset_core::liveness::LivenessState;

use super::state::{AutoConnectState, RuntimeInner};

pub(crate) fn spawn(weak: Weak<RuntimeInner>) -> futures::future::LocalBoxFuture<'static, ()> {
    async move {
        let Some(inner) = weak.upgrade() else { return; };
        let mut hb_rx = inner.auto_connect_chan.rx.borrow_mut().take()
            .expect("auto_connect rx taken once");
        let membership_arc = inner.membership_liveness.clone();
        drop(inner);
        let mut life_sub = membership_arc.subscribe().await;

        loop {
            tokio::select! {
                Some(peer) = hb_rx.recv() => {
                    let Some(inner) = weak.upgrade() else { return; };
                    let mut state = inner.auto_connect_state.borrow_mut();
                    let entry = state.entry(peer.clone()).or_insert(AutoConnectState::Unknown);
                    if *entry == AutoConnectState::Unknown {
                        *entry = AutoConnectState::Dialing;
                        let dialer = inner.dialer.clone();
                        drop(state);
                        drop(inner);
                        dialer.ensure_direct(peer).await;
                    }
                }
                Some(ev) = life_sub.next() => {
                    if ev.state == LivenessState::Stale {
                        let Some(inner) = weak.upgrade() else { return; };
                        let mut state = inner.auto_connect_state.borrow_mut();
                        state.insert(ev.peer.clone(), AutoConnectState::Unknown);
                        drop(state);
                        // Drop per-peer playback resources.
                        inner.frame_sink.drop_peer(&ev.peer);
                        // Drop per-peer jitter buffer too so re-entry starts fresh.
                        inner.jitter.borrow_mut().remove(&ev.peer);
                        inner.last_delivered.borrow_mut().remove(&ev.peer);
                    }
                }
                else => return,
            }
        }
    }.boxed_local()
}
```

Replace stub in `mod.rs`.

- [ ] **Step 4: Run, expect pass**

```
nix develop --command cargo test -p sunset-voice --all-features --test runtime_integration auto_connect_dials
```

Expected: PASS.

- [ ] **Step 5: Add a test for re-dial after Gone**

```rust
#[tokio::test(flavor = "current_thread")]
async fn auto_connect_re_dials_after_gone() {
    // After membership_liveness goes Stale and a fresh heartbeat arrives,
    // ensure_direct must fire again. Use sunset_core::liveness::MockClock
    // to advance past the 5 s window deterministically.
    // (See sunset_core::liveness::tests::mock_clock for the pattern.)
    // Implementation: construct VoiceRuntime with a custom Liveness arc
    // built from MockClock — this requires exposing a `with_liveness`
    // constructor on VoiceRuntime, OR letting the test substitute the
    // arcs after construction. Implementer picks one.
}
```

- [ ] **Step 6: Implement the re-dial path**

To make this testable deterministically, add to `VoiceRuntime`:

```rust
#[cfg(test)]
pub fn test_advance_membership_clock(&self, by: std::time::Duration) {
    // requires Liveness to be built with MockClock — extend constructor
    // to accept an optional clock for tests
}
```

Or simpler: add a helper that lets the test poke the membership_liveness directly. Implementer picks.

- [ ] **Step 7: Commit**

```
git commit -am "sunset-voice: auto-connect FSM dials once per Unknown→Dialing transition"
```

---

### Task 9: Jitter buffer pump

**Files:**
- Create: `crates/sunset-voice/src/runtime/jitter.rs`
- Modify: `crates/sunset-voice/src/runtime/mod.rs`
- Test: `crates/sunset-voice/tests/runtime_integration.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test(flavor = "current_thread")]
async fn jitter_pump_delivers_at_20ms_cadence_and_pads_silence() {
    // Push two frames into the per-peer jitter buffer manually (via
    // a #[cfg(test)] accessor), spawn the pump, observe the FrameSink:
    // - After ~20 ms: 1 frame delivered.
    // - After ~40 ms: 2 frames delivered.
    // - After ~60 ms: 3 deliveries — but third is the *repeat-last* PLC.
    // - After ~80 ms: 4 deliveries — fourth is silence (zeros).
    // - Push another frame → next pump cycle delivers it normally.
    //
    // Implementer: provide `runtime.test_push_frame(peer, pcm)` that
    // bypasses the subscribe loop.
}
```

- [ ] **Step 2: Run, expect failure** — pump is no-op stub.

- [ ] **Step 3: Implement `jitter.rs`**

```rust
//! Per-peer jitter buffer pump. Every 20 ms, for every peer with a
//! non-empty buffer (or a `last_delivered`), pop one frame and call
//! FrameSink::deliver. Underrun → repeat last → silence.

use std::rc::Weak;

use futures::FutureExt;

use crate::FRAME_SAMPLES;
use super::JITTER_PUMP_INTERVAL;
use super::state::RuntimeInner;

pub(crate) fn spawn(weak: Weak<RuntimeInner>) -> futures::future::LocalBoxFuture<'static, ()> {
    async move {
        loop {
            sleep(JITTER_PUMP_INTERVAL).await;
            let Some(inner) = weak.upgrade() else { return; };
            if *inner.deafened.borrow() {
                // Still drain so when un-deafened we don't burst stale frames.
                let mut jitter = inner.jitter.borrow_mut();
                for q in jitter.values_mut() { let _ = q.pop_front(); }
                continue;
            }
            // Snapshot peers to deliver, then deliver outside the borrow.
            let mut to_deliver: Vec<(sunset_sync::PeerId, Vec<f32>)> = Vec::new();
            {
                let mut jitter = inner.jitter.borrow_mut();
                let mut last = inner.last_delivered.borrow_mut();
                for (peer, q) in jitter.iter_mut() {
                    if let Some(frame) = q.pop_front() {
                        last.insert(peer.clone(), frame.clone());
                        to_deliver.push((peer.clone(), frame));
                    } else if let Some(prev) = last.get(peer).cloned() {
                        // Underrun → repeat-last once, then silence on
                        // subsequent pumps. Track per-peer "underrun count"
                        // in last_delivered map to flip to silence.
                        // For simplicity v1: emit silence after the first
                        // repeat (i.e. always emit silence on underrun
                        // unless we just had a real frame).
                        // Implementer: track a per-peer underrun_count;
                        // if 1 → repeat; if ≥2 → silence.
                        to_deliver.push((peer.clone(), prev));
                    }
                }
            }
            for (peer, pcm) in to_deliver {
                inner.frame_sink.deliver(&peer, &pcm);
            }
        }
    }.boxed_local()
}

#[cfg(target_arch = "wasm32")]
async fn sleep(d: std::time::Duration) { wasmtimer::tokio::sleep(d).await; }
#[cfg(not(target_arch = "wasm32"))]
async fn sleep(d: std::time::Duration) { tokio::time::sleep(d).await; }
```

Add `#[cfg(test)] pub fn test_push_frame(&self, peer: sunset_sync::PeerId, pcm: Vec<f32>)` to `VoiceRuntime`.

- [ ] **Step 4: Implement underrun PLC counter properly**

In `state.rs`, replace `last_delivered: RefCell<HashMap<PeerId, Vec<f32>>>` with:

```rust
pub last_delivered: RefCell<HashMap<PeerId, LastDelivered>>,
pub struct LastDelivered { pub pcm: Vec<f32>, pub underruns: u32 }
```

In `jitter.rs` underrun branch:

```rust
} else if let Some(rec) = last.get_mut(peer) {
    rec.underruns = rec.underruns.saturating_add(1);
    let pcm = if rec.underruns == 1 { rec.pcm.clone() } else { vec![0.0_f32; FRAME_SAMPLES] };
    to_deliver.push((peer.clone(), pcm));
}
```

Reset `underruns = 0` whenever a real frame is delivered.

- [ ] **Step 5: Run, expect pass**

- [ ] **Step 6: Commit**

```
git commit -am "sunset-voice: per-peer jitter pump (20ms cadence, repeat-then-silence PLC)"
```

---

### Task 10: Drop-cancellation test

**Files:**
- Test: `crates/sunset-voice/tests/runtime_integration.rs`

- [ ] **Step 1: Write the test**

```rust
#[tokio::test(flavor = "current_thread")]
async fn dropping_runtime_terminates_all_tasks() {
    tokio::task::LocalSet::new().run_until(async {
        let (alice, room) = make_identity_and_room(10);
        let (tx, _) = tokio::sync::broadcast::channel(64);
        let bus: Arc<dyn DynBus> = Arc::new(TestBus { tx, self_pk: alice.store_verifying_key() });
        let dialer: Rc<dyn Dialer> = Rc::new(CountingDialer { calls: Rc::new(RefCell::new(vec![])) });
        let frame_sink: Rc<dyn FrameSink> = Rc::new(RecordingFrameSink {
            delivered: Rc::new(RefCell::new(vec![])),
            dropped: Rc::new(RefCell::new(vec![])),
        });
        let peer_state_sink: Rc<dyn PeerStateSink> = Rc::new(RecordingPeerStateSink {
            events: Rc::new(RefCell::new(vec![])),
        });
        let (runtime, tasks) = VoiceRuntime::new(bus, room, alice, dialer, frame_sink, peer_state_sink);

        let mut handles = vec![];
        handles.push(tokio::task::spawn_local(tasks.heartbeat));
        handles.push(tokio::task::spawn_local(tasks.subscribe));
        handles.push(tokio::task::spawn_local(tasks.combiner));
        handles.push(tokio::task::spawn_local(tasks.auto_connect));
        handles.push(tokio::task::spawn_local(tasks.jitter_pump));

        drop(runtime);
        // Allow each task to observe the upgrade failure.
        tokio::time::sleep(Duration::from_millis(100)).await;
        for h in handles {
            assert!(tokio::time::timeout(Duration::from_millis(500), h).await.is_ok(), "task should finish after Drop");
        }
    }).await;
}
```

- [ ] **Step 2: Run, expect pass (or fail if a task lingers)**

If any task lingers, the implementer must inspect that task's `weak.upgrade()` placement.

- [ ] **Step 3: Commit**

```
git commit -am "sunset-voice: VoiceRuntime Drop cancels all tasks"
```

---

## Phase 2 — `sunset-web-wasm` adapter shell

### Task 11: `BusImpl` implements `DynBus`

**Files:**
- Modify: `crates/sunset-core/src/bus.rs`

The runtime expects `Arc<dyn DynBus>`; provide a blanket `impl DynBus for BusImpl<S, T>`.

- [ ] **Step 1: Write the failing test**

In `crates/sunset-core/tests/bus_integration.rs` (or a new test file):

```rust
#[tokio::test(flavor = "current_thread")]
async fn bus_impl_is_dyn_bus() {
    use sunset_voice::runtime::DynBus;
    let bus: std::sync::Arc<dyn DynBus> = make_test_bus_impl(); // helper that builds BusImpl<MemoryStore, ...>
    let _ = bus;
}
```

(`make_test_bus_impl` follows the pattern in `tests/bus_integration.rs`.)

- [ ] **Step 2: Run, expect failure** — `BusImpl` does not implement `DynBus`.

- [ ] **Step 3: Add the impl in `crates/sunset-core/src/bus.rs`**

```rust
#[async_trait(?Send)]
impl<S, T> sunset_voice::runtime::DynBus for BusImpl<S, T>
where
    S: Store + 'static,
    T: Transport + 'static,
    T::Connection: 'static,
{
    async fn publish_ephemeral(
        &self,
        name: bytes::Bytes,
        payload: bytes::Bytes,
    ) -> Result<(), Box<dyn std::error::Error>> {
        Bus::publish_ephemeral(self, name, payload).await.map_err(|e| Box::new(e) as _)
    }
    async fn subscribe_voice_prefix(
        &self,
        prefix: bytes::Bytes,
    ) -> Result<futures::stream::LocalBoxStream<'static, BusEvent>, Box<dyn std::error::Error>> {
        Bus::subscribe(self, sunset_store::Filter::NamePrefix(prefix)).await.map_err(|e| Box::new(e) as _)
    }
}
```

Add `sunset-voice` to `crates/sunset-core/Cargo.toml` `[dependencies]`. (Yes this creates a sunset-core → sunset-voice dep, which currently flows the opposite way. Verify: `sunset-voice` depends on `sunset-core`. Adding the reverse creates a cycle. **Resolution:** the trait `DynBus` lives in `sunset-voice`, but the impl can live in a third crate that depends on both — e.g. add `crates/sunset-voice/src/runtime/dyn_bus_impl.rs` behind a feature flag and have `sunset-voice` depend on `sunset-core`. The `BusImpl` is concrete and exported by `sunset-core`; impl can therefore live in `sunset-voice/src/runtime/dyn_bus_impl.rs` since `sunset-voice` already depends on `sunset-core`. **Implementer: put the impl in `sunset-voice`, not `sunset-core`.**)

Move the impl to `crates/sunset-voice/src/runtime/dyn_bus_impl.rs`:

```rust
use std::sync::Arc;
use bytes::Bytes;
use async_trait::async_trait;
use futures::stream::LocalBoxStream;
use sunset_core::bus::{Bus, BusEvent, BusImpl};
use sunset_store::{Filter, Store};
use sunset_sync::Transport;

use super::DynBus;

#[async_trait(?Send)]
impl<S, T> DynBus for BusImpl<S, T>
where
    S: Store + 'static,
    T: Transport + 'static,
    T::Connection: 'static,
{
    async fn publish_ephemeral(
        &self,
        name: Bytes,
        payload: Bytes,
    ) -> Result<(), Box<dyn std::error::Error>> {
        Bus::publish_ephemeral(self, name, payload).await.map_err(|e| Box::new(e) as _)
    }
    async fn subscribe_voice_prefix(
        &self,
        prefix: Bytes,
    ) -> Result<LocalBoxStream<'static, BusEvent>, Box<dyn std::error::Error>> {
        Bus::subscribe(self, Filter::NamePrefix(prefix)).await.map_err(|e| Box::new(e) as _)
    }
}
```

Add `pub mod dyn_bus_impl;` to `runtime/mod.rs`.

- [ ] **Step 4: Run, expect pass**

```
nix develop --command cargo test -p sunset-core -p sunset-voice --all-features
```

- [ ] **Step 5: Commit**

```
git commit -am "sunset-voice: blanket DynBus impl for BusImpl"
```

---

### Task 12: `WebDialer` impl wrapping `RoomHandle::connect_direct`

**Files:**
- Create: `crates/sunset-web-wasm/src/voice/dialer.rs`

- [ ] **Step 1: Write the impl** (no separate test — Phase 5 e2e covers it)

```rust
//! `Dialer` impl that wraps the inner sync engine's connect_direct.
//! The voice subsystem owns its own connectivity; the Gleam UI never
//! touches connect_direct.

use std::rc::Rc;
use async_trait::async_trait;

use sunset_sync::PeerId;
use sunset_voice::runtime::Dialer;
use sunset_core::OpenRoom;
use sunset_store::Store;
use sunset_sync::Transport;

pub(crate) struct WebDialer<S: Store + 'static, T: Transport + 'static> {
    pub open_room: Rc<OpenRoom<S, T>>,
}

#[async_trait(?Send)]
impl<S, T> Dialer for WebDialer<S, T>
where
    S: Store + 'static,
    T: Transport + 'static,
    T::Connection: 'static,
{
    async fn ensure_direct(&self, peer: PeerId) {
        if let Err(e) = self.open_room.connect_direct(peer.clone()).await {
            tracing::warn!(peer = ?peer, error = %e, "voice ensure_direct failed");
        }
    }
}
```

- [ ] **Step 2: Add `pub mod dialer;` in `voice/mod.rs`**

- [ ] **Step 3: Build wasm to confirm it compiles**

```
nix develop --command cargo build -p sunset-web-wasm --target wasm32-unknown-unknown --all-features
```

- [ ] **Step 4: Commit**

```
git commit -am "sunset-web-wasm: WebDialer wrapping RoomHandle::connect_direct"
```

---

### Task 13: `WebFrameSink` and `WebPeerStateSink`

**Files:**
- Create: `crates/sunset-web-wasm/src/voice/frame_sink.rs`
- Create: `crates/sunset-web-wasm/src/voice/peer_state_sink.rs`

- [ ] **Step 1: Implement `frame_sink.rs`**

```rust
//! `FrameSink` that calls a JS function with `(peer_id, pcm)` so JS
//! can route to the per-peer playback worklet. Volume is applied
//! browser-side via per-peer GainNode.

use std::cell::RefCell;
use std::rc::Rc;

use js_sys::{Float32Array, Function, Uint8Array};
use wasm_bindgen::JsValue;

use sunset_sync::PeerId;
use sunset_voice::runtime::FrameSink;

pub(crate) struct WebFrameSink {
    pub on_pcm: Rc<RefCell<Option<Function>>>,
    pub on_drop: Rc<RefCell<Option<Function>>>,
}

impl FrameSink for WebFrameSink {
    fn deliver(&self, peer: &PeerId, pcm: &[f32]) {
        if let Some(f) = self.on_pcm.borrow().as_ref() {
            let id = Uint8Array::from(peer.0.as_bytes());
            let arr = Float32Array::from(pcm);
            let _ = f.call2(&JsValue::NULL, &id, &arr);
        }
    }
    fn drop_peer(&self, peer: &PeerId) {
        if let Some(f) = self.on_drop.borrow().as_ref() {
            let id = Uint8Array::from(peer.0.as_bytes());
            let _ = f.call1(&JsValue::NULL, &id);
        }
    }
}
```

- [ ] **Step 2: Implement `peer_state_sink.rs`**

```rust
use std::cell::RefCell;
use std::rc::Rc;
use js_sys::{Function, Uint8Array};
use wasm_bindgen::JsValue;
use sunset_voice::runtime::{PeerStateSink, VoicePeerState};

pub(crate) struct WebPeerStateSink {
    pub handler: Rc<RefCell<Option<Function>>>,
}

impl PeerStateSink for WebPeerStateSink {
    fn emit(&self, state: &VoicePeerState) {
        if let Some(f) = self.handler.borrow().as_ref() {
            let id = Uint8Array::from(state.peer.0.as_bytes());
            let _ = f.apply(&JsValue::NULL, &js_sys::Array::of4(
                &id,
                &JsValue::from_bool(state.in_call),
                &JsValue::from_bool(state.talking),
                &JsValue::from_bool(state.is_muted),
            ));
        }
    }
}
```

- [ ] **Step 3: Build, expect pass**

- [ ] **Step 4: Commit**

```
git commit -am "sunset-web-wasm: WebFrameSink + WebPeerStateSink trait impls"
```

---

### Task 14: Rewire `voice_start` / `voice_stop` / `voice_input` to use `VoiceRuntime`

**Files:**
- Modify: `crates/sunset-web-wasm/src/voice/mod.rs` (collapse to FFI shims)
- Delete: `crates/sunset-web-wasm/src/voice/transport.rs`
- Delete: `crates/sunset-web-wasm/src/voice/subscriber.rs`
- Delete: `crates/sunset-web-wasm/src/voice/liveness.rs`
- Modify: `crates/sunset-web-wasm/src/client.rs` (signature change: drop `on_frame`)

- [ ] **Step 1: Replace `voice/mod.rs`**

```rust
//! Voice subsystem — assembles the runtime + JS-side glue.

mod dialer;
mod frame_sink;
mod peer_state_sink;
#[cfg(feature = "test-hooks")]
mod test_hooks;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use js_sys::{Float32Array, Function};
use wasm_bindgen::prelude::*;

use sunset_core::Identity;
use sunset_voice::runtime::{VoiceRuntime, VoiceTasks};

use crate::room_handle::RoomHandle;

pub(crate) struct ActiveVoice {
    runtime: VoiceRuntime,
    on_pcm: Rc<RefCell<Option<Function>>>,
    on_drop: Rc<RefCell<Option<Function>>>,
    on_state: Rc<RefCell<Option<Function>>>,
}

pub(crate) type VoiceCell = Rc<RefCell<Option<ActiveVoice>>>;
pub(crate) fn new_voice_cell() -> VoiceCell { Rc::new(RefCell::new(None)) }

pub(crate) fn voice_start(
    cell: &VoiceCell,
    identity: &Identity,
    room_handle: &RoomHandle,
    on_pcm: Function,
    on_drop: Function,
    on_state: Function,
) -> Result<(), JsError> {
    if cell.borrow().is_some() {
        return Err(JsError::new("voice already started"));
    }
    let on_pcm_rc = Rc::new(RefCell::new(Some(on_pcm)));
    let on_drop_rc = Rc::new(RefCell::new(Some(on_drop)));
    let on_state_rc = Rc::new(RefCell::new(Some(on_state)));

    let dialer: Rc<dyn sunset_voice::runtime::Dialer> = Rc::new(dialer::WebDialer {
        open_room: room_handle.open_room_rc(),
    });
    let frame_sink: Rc<dyn sunset_voice::runtime::FrameSink> = Rc::new(frame_sink::WebFrameSink {
        on_pcm: on_pcm_rc.clone(),
        on_drop: on_drop_rc.clone(),
    });
    let peer_state_sink: Rc<dyn sunset_voice::runtime::PeerStateSink> = Rc::new(peer_state_sink::WebPeerStateSink {
        handler: on_state_rc.clone(),
    });
    let bus: Arc<dyn sunset_voice::runtime::DynBus> = room_handle.bus_arc_dyn();

    let (runtime, tasks) = VoiceRuntime::new(
        bus,
        room_handle.room_rc(),
        identity.clone(),
        dialer,
        frame_sink,
        peer_state_sink,
    );

    wasm_bindgen_futures::spawn_local(tasks.heartbeat);
    wasm_bindgen_futures::spawn_local(tasks.subscribe);
    wasm_bindgen_futures::spawn_local(tasks.combiner);
    wasm_bindgen_futures::spawn_local(tasks.auto_connect);
    wasm_bindgen_futures::spawn_local(tasks.jitter_pump);

    *cell.borrow_mut() = Some(ActiveVoice {
        runtime,
        on_pcm: on_pcm_rc,
        on_drop: on_drop_rc,
        on_state: on_state_rc,
    });
    Ok(())
}

pub(crate) fn voice_stop(cell: &VoiceCell) -> Result<(), JsError> {
    *cell.borrow_mut() = None;
    Ok(())
}

pub(crate) fn voice_input(cell: &VoiceCell, pcm: &Float32Array) -> Result<(), JsError> {
    let slot = cell.borrow();
    let v = slot.as_ref().ok_or_else(|| JsError::new("voice not started"))?;
    let mut buf = vec![0.0_f32; sunset_voice::FRAME_SAMPLES];
    if pcm.length() as usize != sunset_voice::FRAME_SAMPLES {
        return Err(JsError::new("voice_input: wrong frame size"));
    }
    pcm.copy_to(&mut buf);
    v.runtime.send_pcm(&buf);
    Ok(())
}

pub(crate) fn voice_set_muted(cell: &VoiceCell, muted: bool) {
    if let Some(v) = cell.borrow().as_ref() { v.runtime.set_muted(muted); }
}
pub(crate) fn voice_set_deafened(cell: &VoiceCell, deafened: bool) {
    if let Some(v) = cell.borrow().as_ref() { v.runtime.set_deafened(deafened); }
}
```

- [ ] **Step 2: Add helpers to `RoomHandle`**

In `crates/sunset-web-wasm/src/room_handle.rs`, add:

```rust
impl RoomHandle {
    pub(crate) fn open_room_rc(&self) -> Rc<sunset_core::OpenRoom<...>> { self.inner.clone() }
    pub(crate) fn room_rc(&self) -> Rc<sunset_core::Room> { /* extract from OpenRoom */ }
    pub(crate) fn bus_arc_dyn(&self) -> Arc<dyn sunset_voice::runtime::DynBus> { /* upcast the BusImpl Arc */ }
}
```

(Implementer figures out exact types based on what `RoomHandle` already holds.)

- [ ] **Step 3: Delete the old voice modules**

```
git rm crates/sunset-web-wasm/src/voice/transport.rs
git rm crates/sunset-web-wasm/src/voice/subscriber.rs
git rm crates/sunset-web-wasm/src/voice/liveness.rs
```

- [ ] **Step 4: Update `crates/sunset-web-wasm/src/client.rs`**

Change `voice_start` signature:

```rust
pub fn voice_start(
    &self,
    room_name: String,
    on_pcm: js_sys::Function,
    on_drop_peer: js_sys::Function,
    on_voice_peer_state: js_sys::Function,
) -> Result<(), JsError> {
    // Look up the open RoomHandle by name; if absent, error.
    // RoomHandles are stored on the Peer (per-room registry).
    // Implementer: add a `Peer::room_handle(name)` accessor or
    // require the caller to hold the RoomHandle and pass it in.
    let handle = self.inner.room_handle(&room_name)
        .ok_or_else(|| JsError::new("room not open; call open_room first"))?;
    crate::voice::voice_start(&self.voice, &self.identity, &handle, on_pcm, on_drop_peer, on_voice_peer_state)
}

pub fn voice_set_muted(&self, muted: bool) { crate::voice::voice_set_muted(&self.voice, muted); }
pub fn voice_set_deafened(&self, deafened: bool) { crate::voice::voice_set_deafened(&self.voice, deafened); }
```

(Note: a `room_handle()` accessor on `Peer` may need to be added. Since the existing `open_room` returns a `RoomHandle`, an alternative is moving voice_start onto `RoomHandle` itself. **Implementer pick**: cleanest is to move `voice_start` onto `RoomHandle` — it's a per-room operation. Update the FFI accordingly. The harness page already expects `voice_start(room_name, ...)` on `Client`, so a transitional shim on `Client` that looks up the handle is acceptable.)

- [ ] **Step 5: Build wasm**

```
nix develop --command cargo build -p sunset-web-wasm --target wasm32-unknown-unknown --all-features
```

Fix any compile errors. Update `voice-e2e-test.html` later (Task 22).

- [ ] **Step 6: Commit**

```
git commit -am "sunset-web-wasm: voice/ becomes a thin shell over VoiceRuntime"
```

---

### Task 15: Per-peer GainNode + `voice_set_peer_volume` FFI

**Files:**
- Modify: `crates/sunset-web-wasm/src/voice/mod.rs` (record per-peer gain in JS callback path)
- Modify: `crates/sunset-web-wasm/src/client.rs` (FFI)

The actual GainNode lives JS-side (browser audio graph). Rust just stores the desired gain per peer and forwards via a JS callback registered at `voice_start`. Skip in Rust if no JS callback is set.

- [ ] **Step 1: Add `on_set_peer_volume` parameter to `voice_start`** (4th JS callback)

In `voice/mod.rs`, extend `ActiveVoice` to hold `on_set_peer_volume: Rc<RefCell<Option<Function>>>` and pending per-peer gains map:

```rust
pending_gains: RefCell<HashMap<PeerId, f32>>,
```

- [ ] **Step 2: Implement**

```rust
pub(crate) fn voice_set_peer_volume(cell: &VoiceCell, peer_bytes: &[u8], gain: f32) {
    let Some(v) = cell.borrow().as_ref() else { return; };
    if peer_bytes.len() != 32 { return; }
    let pk = sunset_store::VerifyingKey::from_bytes(peer_bytes.try_into().unwrap());
    let peer = sunset_sync::PeerId(pk);
    if let Some(f) = v.on_set_peer_volume.borrow().as_ref() {
        let id = js_sys::Uint8Array::from(peer.0.as_bytes());
        let _ = f.call2(&JsValue::NULL, &id, &JsValue::from_f64(gain as f64));
    }
}
```

- [ ] **Step 3: Add the FFI**

```rust
#[wasm_bindgen]
impl Client {
    pub fn voice_set_peer_volume(&self, peer_id: &[u8], gain: f32) {
        crate::voice::voice_set_peer_volume(&self.voice, peer_id, gain);
    }
}
```

- [ ] **Step 4: Build, commit**

```
git commit -am "sunset-web-wasm: voice_set_peer_volume FFI (browser handles GainNode)"
```

---

### Task 16: `feature = "test-hooks"` — `voice_inject_pcm`, frame recorder, `voice_active_peers`

**Files:**
- Modify: `crates/sunset-web-wasm/Cargo.toml`
- Create: `crates/sunset-web-wasm/src/voice/test_hooks.rs`
- Modify: `crates/sunset-web-wasm/src/client.rs`

- [ ] **Step 1: Add the feature**

In `Cargo.toml`:

```toml
[features]
default = []
test-hooks = []
```

- [ ] **Step 2: Implement `test_hooks.rs`**

```rust
//! Test-only FFI: synthetic PCM injection + frame recorder. Compiled
//! out of production WASM.

use std::cell::RefCell;
use std::rc::Rc;
use js_sys::{Float32Array, Uint8Array};
use wasm_bindgen::JsValue;

use sunset_sync::PeerId;
use sunset_voice::runtime::{FrameSink, VoicePeerState};

const RING_PER_PEER: usize = 1024;

#[derive(Clone, Default)]
pub struct RecordedFrame {
    pub seq_in_frame: i32,
    pub len: u32,
    pub checksum: String,
}

pub struct RecorderInner {
    pub frames: std::collections::HashMap<PeerId, std::collections::VecDeque<RecordedFrame>>,
}

pub struct RecordingFrameSink {
    pub inner: Rc<RefCell<RecorderInner>>,
    pub forward: Rc<dyn FrameSink>,
}

impl FrameSink for RecordingFrameSink {
    fn deliver(&self, peer: &PeerId, pcm: &[f32]) {
        let seq = decode_counter(pcm);
        let mut sha = sha2::Sha256::new();
        use sha2::Digest;
        for s in pcm { sha.update(s.to_le_bytes()); }
        let checksum = hex::encode(sha.finalize());
        let mut g = self.inner.borrow_mut();
        let q = g.frames.entry(peer.clone()).or_default();
        if q.len() >= RING_PER_PEER { q.pop_front(); }
        q.push_back(RecordedFrame { seq_in_frame: seq, len: pcm.len() as u32, checksum });
        drop(g);
        self.forward.deliver(peer, pcm);
    }
    fn drop_peer(&self, peer: &PeerId) { self.forward.drop_peer(peer); }
}

/// Encode counter `c` into the first sample of an otherwise-deterministic
/// PCM frame: sample[0] = (c as f32) / 1e6 (scale leaves 24 bits of
/// counter range — plenty for tests). Remaining samples follow a
/// deterministic per-counter pattern so checksums differ across counter
/// values.
pub fn synth_pcm_with_counter(counter: i32) -> Vec<f32> {
    let mut v = vec![0.0_f32; sunset_voice::FRAME_SAMPLES];
    v[0] = (counter as f32) / 1_000_000.0;
    for i in 1..v.len() {
        v[i] = ((counter.wrapping_add(i as i32) as f32) / 1_000_000.0).sin();
    }
    v
}

pub fn decode_counter(pcm: &[f32]) -> i32 {
    if pcm.is_empty() { return -1; }
    (pcm[0] * 1_000_000.0).round() as i32
}
```

- [ ] **Step 3: Wire to `Client` FFI**

```rust
#[cfg(feature = "test-hooks")]
#[wasm_bindgen]
impl Client {
    pub fn voice_inject_pcm(&self, pcm: &Float32Array) -> Result<(), JsError> {
        crate::voice::voice_input(&self.voice, pcm)
    }

    pub fn voice_install_frame_recorder(&self) -> Result<(), JsError> {
        crate::voice::install_recorder(&self.voice)
    }

    pub fn voice_recorded_frames(&self, peer_id: &[u8]) -> Result<JsValue, JsError> {
        crate::voice::recorded_frames(&self.voice, peer_id)
    }

    pub fn voice_active_peers(&self) -> Result<JsValue, JsError> {
        crate::voice::active_peers(&self.voice)
    }
}
```

Implement the helpers in `voice/mod.rs` (gate on `#[cfg(feature = "test-hooks")]`). `install_recorder` swaps the `WebFrameSink` inside `ActiveVoice` with a `RecordingFrameSink` that forwards to it.

(`voice_active_peers` returns the `last_emitted` snapshot — add a `#[cfg(feature = "test-hooks")] pub fn snapshot_states(&self)` on `VoiceRuntime`.)

- [ ] **Step 4: Update the build to enable the feature for e2e**

In `web/Makefile` or wherever the WASM is built for the dev server, add `--features sunset-web-wasm/test-hooks` for the test build path. For now, document in the plan: the e2e Playwright runs build with `cargo build -p sunset-web-wasm --target wasm32-unknown-unknown --features test-hooks`.

- [ ] **Step 5: Build, commit**

```
git commit -am "sunset-web-wasm: test-hooks feature — voice_inject_pcm + frame recorder"
```

---

## Phase 3 — Browser audio glue + harness page update

### Task 17: JS-side per-peer playback worklet table + GainNode

**Files:**
- Create: `web/src/sunset_web/voice.ffi.mjs`
- Modify: `web/audio/voice-playback-worklet.js` (no change expected; one instance per peer)

- [ ] **Step 1: Implement `voice.ffi.mjs`**

```js
// JS-side voice wiring. Owns:
// - the AudioContext (created lazily on first start)
// - per-peer { workletNode, gainNode } table
// - capture worklet stream
// - GainNode value updates from voice_set_peer_volume

let ctx = null;
const peers = new Map(); // peerHex -> { worklet, gain }
let captureNode = null;
let captureStream = null;

export function ensureCtx() {
  if (!ctx) ctx = new AudioContext({ sampleRate: 48000 });
  return ctx;
}

export async function startCapture(client) {
  await ensureCtx();
  await ctx.audioWorklet.addModule("/audio/voice-capture-worklet.js");
  await ctx.audioWorklet.addModule("/audio/voice-playback-worklet.js");
  captureStream = await navigator.mediaDevices.getUserMedia({
    audio: { echoCancellation: true, noiseSuppression: true, autoGainControl: true, channelCount: 1 },
  });
  const src = ctx.createMediaStreamSource(captureStream);
  captureNode = new AudioWorkletNode(ctx, "voice-capture");
  captureNode.port.onmessage = (e) => {
    if (e.data instanceof Float32Array && e.data.length === 960) {
      try { client.voice_input(e.data); } catch (err) { console.warn("voice_input failed", err); }
    }
  };
  src.connect(captureNode);
}

export function stopCapture() {
  if (captureStream) { for (const t of captureStream.getTracks()) t.stop(); captureStream = null; }
  captureNode = null;
  for (const [peer, slot] of peers) {
    try { slot.worklet.disconnect(); slot.gain.disconnect(); } catch {}
  }
  peers.clear();
}

export function deliverFrame(peerHex, pcm) {
  if (!ctx) return;
  let slot = peers.get(peerHex);
  if (!slot) {
    const w = new AudioWorkletNode(ctx, "voice-playback");
    const g = ctx.createGain();
    g.gain.value = 1.0;
    w.connect(g).connect(ctx.destination);
    slot = { worklet: w, gain: g };
    peers.set(peerHex, slot);
  }
  slot.worklet.port.postMessage(pcm, [pcm.buffer]);
}

export function dropPeer(peerHex) {
  const slot = peers.get(peerHex);
  if (!slot) return;
  try { slot.worklet.disconnect(); slot.gain.disconnect(); } catch {}
  peers.delete(peerHex);
}

export function setPeerVolume(peerHex, gain) {
  const slot = peers.get(peerHex);
  if (!slot) return;
  slot.gain.gain.value = Math.max(0, Math.min(2.0, gain));
}

export function getPeerGain(peerHex) {
  const slot = peers.get(peerHex);
  return slot ? slot.gain.gain.value : null;
}
```

- [ ] **Step 2: Commit**

```
git commit -am "web/voice.ffi.mjs: capture + per-peer playback + GainNode"
```

---

### Task 18: Update `voice-e2e-test.html` to use frame recorder

**Files:**
- Modify: `web/voice-e2e-test.html`

- [ ] **Step 1: Rewrite the harness**

Replace `incoming` map and `framesFor` with:

```js
window.__voice = {
  async start({ seed, room, relay }) {
    await init();
    const bytes = new Uint8Array(seed.match(/.{2}/g).map((b) => parseInt(b, 16)));
    const client = new Client(bytes);
    window.__voice.client = client;
    window.__voice.room = room;
    window.__voice.publicKeyHex = hex(new Uint8Array(client.public_key));
    await client.add_relay(relay);
    const roomHandle = await client.open_room(room);
    window.__voice.roomHandle = roomHandle;
    return { publicKey: window.__voice.publicKeyHex };
  },
  async startVoice() {
    // Install recorder before start so we capture from the first frame.
    window.__voice.client.voice_install_frame_recorder();
    window.__voice.client.voice_start(
      window.__voice.room,
      // on_pcm: ignored in harness — recorder captures
      () => {},
      // on_drop: ignored
      () => {},
      // on_voice_peer_state
      (peerId, in_call, talking, is_muted) => {
        const k = hex(new Uint8Array(peerId));
        window.__voice._stateMap.set(k, { in_call, talking, is_muted });
      },
    );
  },
  recordedFor(hexPeerId) {
    const bytes = hexBytes(hexPeerId);
    return window.__voice.client.voice_recorded_frames(bytes);
  },
  stop() { window.__voice.client.voice_stop(); },
  injectPcm(samples) {
    window.__voice.client.voice_inject_pcm(new Float32Array(samples));
  },
  // ... voice_state, conn_state same as before
};
window.__voice._stateMap = new Map();

function hexBytes(s) { return new Uint8Array(s.match(/.{2}/g).map((b) => parseInt(b, 16))); }
```

- [ ] **Step 2: Commit**

```
git commit -am "voice-e2e-test.html: use frame recorder; drop on_frame callback"
```

---

### Task 19: Rename + slim `voice_network.spec.js` → `voice_protocol.spec.js`

**Files:**
- Rename: `web/e2e/voice_network.spec.js` → `web/e2e/voice_protocol.spec.js`
- Modify: the renamed file

- [ ] **Step 1: Move the file**

```
git mv web/e2e/voice_network.spec.js web/e2e/voice_protocol.spec.js
```

- [ ] **Step 2: Slim it down**

Keep only the byte-equal frame round-trip test. Use `injectPcm` + `recordedFor` instead of `sendFrame` + `framesFor`. Remove the manual `connectDirect` call (auto-connect now does it). Verify the recorded frame's checksum matches the expected checksum for the injected synthetic PCM.

- [ ] **Step 3: Run the test**

```
nix develop --command sh -c "cd web && npx playwright test e2e/voice_protocol.spec.js"
```

Expected: PASS (assuming Phase 1 + 2 land cleanly).

- [ ] **Step 4: Commit**

```
git commit -am "voice_protocol.spec: byte-equal regression via frame recorder"
```

---

## Phase 4 — Gleam UI wiring

### Task 20: Gleam FFI bindings for voice

**Files:**
- Create: `web/src/sunset_web/voice.gleam`

- [ ] **Step 1: Write the bindings**

```gleam
//// FFI bindings for the voice subsystem. Wraps the JS-side
//// voice.ffi.mjs and the wasm Client voice methods.

import gleam/javascript/promise.{type Promise}

pub type VoiceClient

@external(javascript, "./voice.ffi.mjs", "ensureCtx")
pub fn ensure_audio_context() -> Nil

@external(javascript, "./voice.ffi.mjs", "startCapture")
pub fn start_capture(client: VoiceClient) -> Promise(Nil)

@external(javascript, "./voice.ffi.mjs", "stopCapture")
pub fn stop_capture() -> Nil

@external(javascript, "./voice.ffi.mjs", "deliverFrame")
pub fn deliver_frame(peer_hex: String, pcm: Float32Array) -> Nil

pub type Float32Array

@external(javascript, "./voice.ffi.mjs", "dropPeer")
pub fn drop_peer(peer_hex: String) -> Nil

@external(javascript, "./voice.ffi.mjs", "setPeerVolume")
pub fn set_peer_volume(peer_hex: String, gain: Float) -> Nil

// Wasm Client wrappers (call into the wasm export bindings)
@external(javascript, "./voice.ffi.mjs", "wasmVoiceStart")
pub fn voice_start(client: VoiceClient, room: String) -> Promise(Result(Nil, String))

@external(javascript, "./voice.ffi.mjs", "wasmVoiceStop")
pub fn voice_stop(client: VoiceClient) -> Nil

@external(javascript, "./voice.ffi.mjs", "wasmVoiceSetMuted")
pub fn voice_set_muted(client: VoiceClient, muted: Bool) -> Nil

@external(javascript, "./voice.ffi.mjs", "wasmVoiceSetDeafened")
pub fn voice_set_deafened(client: VoiceClient, deafened: Bool) -> Nil
```

In `voice.ffi.mjs` add:

```js
export async function wasmVoiceStart(client, room) {
  try {
    await startCapture(client);
    client.voice_start(
      room,
      (peerId, pcm) => {
        const hex = uint8ToHex(new Uint8Array(peerId));
        deliverFrame(hex, new Float32Array(pcm));
      },
      (peerId) => {
        const hex = uint8ToHex(new Uint8Array(peerId));
        dropPeer(hex);
      },
      (peerId, inCall, talking, isMuted) => {
        const hex = uint8ToHex(new Uint8Array(peerId));
        // Forward to a registered Gleam callback; see Task 21.
        if (window.__voicePeerStateHandler) {
          window.__voicePeerStateHandler(hex, inCall, talking, isMuted);
        }
      },
    );
    return { Ok: null };
  } catch (e) {
    return { Error: String(e?.message || e) };
  }
}
export function wasmVoiceStop(client) { try { client.voice_stop(); } catch {}; stopCapture(); }
export function wasmVoiceSetMuted(client, m) { client.voice_set_muted(!!m); }
export function wasmVoiceSetDeafened(client, d) { client.voice_set_deafened(!!d); }
function uint8ToHex(a) { return Array.from(a).map((b) => b.toString(16).padStart(2, "0")).join(""); }
```

- [ ] **Step 2: Build gleam, fix syntax errors**

```
nix develop --command sh -c "cd web && gleam build"
```

- [ ] **Step 3: Commit**

```
git commit -am "web: gleam FFI bindings for voice"
```

---

### Task 21: `VoiceModel` + `Msg` variants

**Files:**
- Modify: `web/src/sunset_web/domain.gleam`
- Modify: `web/src/sunset_web.gleam`

- [ ] **Step 1: Add `VoicePeerStateUI` to `domain.gleam`**

```gleam
pub type VoicePeerStateUI {
  VoicePeerStateUI(in_call: Bool, talking: Bool, is_muted: Bool)
}

pub type VoiceModel {
  VoiceModel(
    self_in_call: Option(RoomId),
    self_muted: Bool,
    self_deafened: Bool,
    peers: Dict(String, VoicePeerStateUI),  // peer hex → state
    permission_error: Option(String),
  )
}
```

- [ ] **Step 2: Add `voice` field on the Lustre `Model` in `sunset_web.gleam`**

```gleam
voice: domain.VoiceModel,
```

Initialize: `voice: VoiceModel(self_in_call: None, self_muted: False, self_deafened: False, peers: dict.new(), permission_error: None)`.

- [ ] **Step 3: Add `Msg` variants**

```gleam
pub type Msg {
  // ... existing
  JoinVoice(RoomId)
  LeaveVoice
  ToggleSelfMute
  ToggleSelfDeafen
  VoicePeerStateChanged(peer_hex: String, in_call: Bool, talking: Bool, is_muted: Bool)
  SetPeerVolume(peer_hex: String, gain: Float)
  ToggleMuteForPeer(peer_hex: String)
  VoicePermissionDenied(message: String)
  ResetVoiceError
}
```

- [ ] **Step 4: Implement `update` cases for each new Msg**

Each one updates `model.voice` and calls the appropriate FFI. Wire the `window.__voicePeerStateHandler` from JS to a `Msg` dispatch via Lustre's effect system or a global event handler.

- [ ] **Step 5: Build, commit**

```
git commit -am "web: VoiceModel + Msg variants for voice control"
```

---

### Task 22: Wire join/leave to voice channel row click

**Files:**
- Modify: `web/src/sunset_web/views/channels.gleam` (voice channel row clickable)
- Modify: `web/src/sunset_web.gleam` (handler)

- [ ] **Step 1: Add `on_click` to the voice channel row**

In `channels.gleam` modify `live_voice_row` and `idle_voice_row` to accept `on_join: msg` and `on_leave: msg` and emit them on click.

- [ ] **Step 2: Wire the `RoomId` in the call site**

In `sunset_web.gleam`, pass `JoinVoice(room.id)` / `LeaveVoice` as appropriate based on `model.voice.self_in_call`.

- [ ] **Step 3: Implement `JoinVoice` handler**

```gleam
JoinVoice(room_id) -> {
  let assert Some(client) = model.client
  let promise = voice.voice_start(client, room_id_to_string(room_id))
  // Use lustre/effect to dispatch the result
  #(model, promise_to_effect(promise, fn(r) {
    case r {
      Ok(_) -> Noop
      Error(msg) -> VoicePermissionDenied(msg)
    }
  }))
}
```

Update `model.voice.self_in_call = Some(room_id)` only on Ok (or eagerly with rollback on Error — implementer pick; spec says rollback).

- [ ] **Step 4: Build, manually verify** (start dev server, click voice channel, confirm `voice_start` runs)

- [ ] **Step 5: Commit**

```
git commit -am "web: voice channel row click toggles join/leave"
```

---

### Task 23: Wire mic mute, deafen, leave on minibar/self-controls

**Files:**
- Modify: `web/src/sunset_web/views/voice_minibar.gleam`
- Modify: `web/src/sunset_web/views/channels.gleam` (self_control_bar)
- Modify: `web/src/sunset_web.gleam`

- [ ] **Step 1: Wire mic icon → `ToggleSelfMute`**

In `voice_minibar.gleam`, the `icon_button("Mute mic", ...)` gains `event.on_click(on_mute)` and the view function takes an `on_mute: msg` parameter.

- [ ] **Step 2: Same for deafen, leave**

- [ ] **Step 3: Update handlers in `sunset_web.gleam`**

```gleam
ToggleSelfMute -> {
  let new_muted = !model.voice.self_muted
  voice.voice_set_muted(client, new_muted)
  let voice = VoiceModel(..model.voice, self_muted: new_muted)
  #(Model(..model, voice), effect.none())
}
```

(Same shape for deafen and leave.)

- [ ] **Step 4: Build, commit**

```
git commit -am "web: minibar / self-controls wire to voice FFI"
```

---

### Task 24: Wire per-peer volume + mute-for-me in popover

**Files:**
- Modify: `web/src/sunset_web/views/voice_popover.gleam`
- Modify: `web/src/sunset_web.gleam`

- [ ] **Step 1: Volume slider emits `SetPeerVolume(peer_hex, gain)`**

The slider's `on_set_volume(int)` callback already exists; route it to `SetPeerVolume`.

- [ ] **Step 2: Mute-for-me toggle**

When toggled on, dispatch `ToggleMuteForPeer(peer_hex)`. Update handler:

```gleam
ToggleMuteForPeer(peer_hex) -> {
  let settings = member_voice_settings(model.voice_settings, peer_hex)
  let new_muted_for_me = !settings.muted_for_me
  let new_settings = if new_muted_for_me {
    VoiceSettings(..settings, muted_for_me: True, prior_volume: settings.volume)
  } else {
    VoiceSettings(..settings, muted_for_me: False, volume: settings.prior_volume)
  }
  let gain = if new_muted_for_me { 0.0 } else { int_to_float(new_settings.volume) /. 100.0 }
  voice.set_peer_volume(peer_hex, gain)
  // Update model.voice_settings
  ...
}
```

(Add `prior_volume: Int` to `VoiceSettings` if not already present.)

- [ ] **Step 3: Build, commit**

```
git commit -am "web: voice popover volume + mute-for-me wired to FFI"
```

---

### Task 25: Wire `on_voice_peer_state` callback into Lustre dispatch

**Files:**
- Modify: `web/src/sunset_web/voice.ffi.mjs`
- Modify: `web/src/sunset_web.gleam`

- [ ] **Step 1: Set `window.__voicePeerStateHandler` from Gleam**

Lustre's effect system can register a global callback. Use a custom effect:

```gleam
fn install_voice_state_handler(dispatch: fn(Msg) -> Nil) -> Effect(Msg) {
  effect.from(fn(d) {
    install_voice_state_handler_ffi(fn(hex, in_call, talking, is_muted) {
      d(VoicePeerStateChanged(hex, in_call, talking, is_muted))
    })
  })
}
```

In `voice.ffi.mjs`:

```js
export function installVoiceStateHandler(cb) {
  window.__voicePeerStateHandler = cb;
}
```

Run this once at app init.

- [ ] **Step 2: Implement `VoicePeerStateChanged` handler**

```gleam
VoicePeerStateChanged(hex, in_call, talking, is_muted) -> {
  let new_peers = dict.insert(model.voice.peers, hex, VoicePeerStateUI(in_call, talking, is_muted))
  let voice = VoiceModel(..model.voice, peers: new_peers)
  #(Model(..model, voice), effect.none())
}
```

- [ ] **Step 3: Replace fixture-backed in_call/talking with `model.voice.peers`**

In `channels.gleam` `live_voice_row` and `voice_popover.gleam`, derive member badges from `model.voice.peers[hex]` instead of `m.in_call`/`m.talking`. Implementer maps `peer_hex` ↔ `MemberId` via the existing membership tracker.

- [ ] **Step 4: Build, manually verify in two browser tabs**

- [ ] **Step 5: Commit**

```
git commit -am "web: real voice peer state drives in_call/talking/muted indicators"
```

---

### Task 26: Mic permission denied → toast

**Files:**
- Modify: `web/src/sunset_web.gleam`
- Add minimal toast component if not present

- [ ] **Step 1: Implement `VoicePermissionDenied(msg)` handler**

```gleam
VoicePermissionDenied(msg) -> {
  let voice = VoiceModel(..model.voice, self_in_call: None, permission_error: Some("Microphone access required to join voice."))
  #(Model(..model, voice), effect.none())
}
```

- [ ] **Step 2: Render the toast when `permission_error` is `Some`**

If no toast component exists, add a minimal `voice_error_toast.gleam` view: floats top-right, has a close button → `ResetVoiceError`.

- [ ] **Step 3: Implement `ResetVoiceError` to clear**

- [ ] **Step 4: Build, commit**

```
git commit -am "web: mic permission denied surfaces a toast and rolls back"
```

---

## Phase 5 — Real Gleam UI Playwright tests

### Task 27: Test helpers (`web/e2e/helpers/voice.js`)

**Files:**
- Create: `web/e2e/helpers/voice.js`

- [ ] **Step 1: Implement helpers**

```js
import { spawn } from "child_process";
import { mkdtempSync, rmSync } from "fs";
import { tmpdir } from "os";
import { join } from "path";
import { writeFileSync } from "fs";

export async function spawnRelay() {
  const dir = mkdtempSync(join(tmpdir(), "sunset-relay-voice-"));
  const cfg = join(dir, "relay.toml");
  writeFileSync(cfg, [
    `listen_addr = "127.0.0.1:0"`,
    `data_dir = "${dir}"`,
    `interest_filter = "all"`,
    `identity_secret = "auto"`,
    `peers = []`,
    "",
  ].join("\n"));
  const proc = spawn("sunset-relay", ["--config", cfg], { stdio: ["ignore", "pipe", "pipe"] });
  const addr = await new Promise((res, rej) => {
    const t = setTimeout(() => rej(new Error("relay no banner in 15s")), 15000);
    let buf = "";
    proc.stdout.on("data", (c) => {
      buf += c.toString();
      const m = buf.match(/address:\s+(ws:\/\/[^\s]+)/);
      if (m) { clearTimeout(t); res(m[1]); }
    });
    proc.stderr.on("data", (c) => process.stderr.write(`[relay] ${c}`));
    proc.on("exit", (c) => { if (c !== null && c !== 0) { clearTimeout(t); rej(new Error(`relay exited ${c}`)); } });
  });
  return { proc, dir, addr };
}

export function teardownRelay(state) {
  if (state.proc?.exitCode === null) state.proc.kill("SIGTERM");
  if (state.dir) rmSync(state.dir, { recursive: true, force: true });
}

export function freshSeedHex() {
  let s = "";
  for (let i = 0; i < 64; i++) s += Math.floor(Math.random() * 16).toString(16);
  return s;
}

export function syntheticPcm(counter) {
  const pcm = new Float32Array(960);
  pcm[0] = counter / 1_000_000;
  for (let i = 1; i < 960; i++) pcm[i] = Math.sin(((counter + i) / 1_000_000));
  return pcm;
}

export function decodeCounter(firstSampleVal) {
  return Math.round(firstSampleVal * 1_000_000);
}
```

- [ ] **Step 2: Commit**

```
git commit -am "web/e2e: helpers for voice tests (relay spawn, synthetic PCM)"
```

---

### Task 28: `voice_two_way.spec.js`

**Files:**
- Create: `web/e2e/voice_two_way.spec.js`
- Modify: `web/playwright.config.js` (grant `microphone` permission)

- [ ] **Step 1: Update `playwright.config.js`**

```js
projects: [
  {
    name: "chromium",
    use: { ...devices["Desktop Chrome"], permissions: ["microphone"] },
  },
],
```

- [ ] **Step 2: Write the test**

```js
import { test, expect } from "@playwright/test";
import { spawnRelay, teardownRelay, freshSeedHex, syntheticPcm, decodeCounter } from "./helpers/voice.js";

let relay;
test.beforeAll(async () => { relay = await spawnRelay(); });
test.afterAll(async () => { teardownRelay(relay); });

test("alice + bob hear each other through real Gleam UI", async ({ browser }) => {
  const aliceCtx = await browser.newContext({ permissions: ["microphone"] });
  const bobCtx = await browser.newContext({ permissions: ["microphone"] });
  const alice = await aliceCtx.newPage();
  const bob = await bobCtx.newPage();

  // Use a deterministic room name that maps to a single voice channel.
  await alice.goto(`/?relay=${encodeURIComponent(relay.addr)}#voice-test`);
  await bob.goto(`/?relay=${encodeURIComponent(relay.addr)}#voice-test`);

  // Inject seeds via a query param or localStorage hook to make
  // identities fresh per test (matches voice_protocol.spec pattern).
  await alice.evaluate((s) => localStorage.setItem("sunset.seed", s), freshSeedHex());
  await bob.evaluate((s) => localStorage.setItem("sunset.seed", s), freshSeedHex());
  await alice.reload();
  await bob.reload();

  // Wait for both to see each other in the member rail.
  await expect(alice.getByTestId(`member-row-${await bob.evaluate(() => window.__client?.public_key_hex)}`)).toBeVisible({ timeout: 10000 });
  // (Implementer: ensure the Gleam UI renders a `member-row-<hex>` testid.)

  // Alice clicks join voice — minibar should appear within 500ms.
  await alice.locator('[data-testid="voice-channel-row"]').click();
  await expect(alice.locator('[data-testid="voice-minibar"]')).toBeVisible({ timeout: 500 });

  // Bob joins voice.
  await bob.locator('[data-testid="voice-channel-row"]').click();

  // Both should show "2 in call" within 2s (use voice channel's badge).
  await expect(alice.locator('[data-testid="voice-channel-row"]')).toContainText("2", { timeout: 2000 });
  await expect(bob.locator('[data-testid="voice-channel-row"]')).toContainText("2", { timeout: 2000 });

  // Alice injects 50 frames over 1s.
  await alice.evaluate(async () => {
    window.__client.voice_install_frame_recorder();
  });
  await bob.evaluate(async () => {
    window.__client.voice_install_frame_recorder();
  });
  for (let c = 1; c <= 50; c++) {
    const pcm = syntheticPcm(c);
    await alice.evaluate((arr) => window.__client.voice_inject_pcm(new Float32Array(arr)), Array.from(pcm));
    await alice.waitForTimeout(20);
  }

  // Within 3s, bob's recorder shows ≥ 40 frames from alice.
  const alicePk = await alice.evaluate(() => window.__client.public_key_hex);
  const recorded = await bob.waitForFunction(
    (pk) => {
      const arr = window.__client.voice_recorded_frames(new Uint8Array(pk.match(/.{2}/g).map((b) => parseInt(b, 16))));
      return arr.length >= 40 ? arr : null;
    },
    alicePk,
    { timeout: 3000 },
  );
  const frames = await recorded.jsonValue();
  // Counter monotonic + no run of 5 identical
  let prev = -1;
  let runLen = 0;
  for (const f of frames) {
    expect(f.seq_in_frame).toBeGreaterThanOrEqual(prev);
    if (f.seq_in_frame === prev) runLen++; else runLen = 0;
    expect(runLen).toBeLessThan(5);
    prev = f.seq_in_frame;
  }
});
```

(Implementer: a `__client` global on `window` is needed; this is a small dev affordance. Add it in the Gleam app's init code: `window.__client = client;`.)

- [ ] **Step 3: Run, expect pass**

```
nix develop --command sh -c "cd web && npx playwright test voice_two_way"
```

- [ ] **Step 4: Commit**

```
git commit -am "e2e: voice_two_way (UI join + content-checked frames)"
```

---

### Task 29: `voice_three_way.spec.js`

**Files:**
- Create: `web/e2e/voice_three_way.spec.js`

- [ ] **Step 1: Write the test** — same shape as 2-way but with three pages. Assert "3 in call" within 4s of the third joining; alice injects → both bob and carol pass content checks; carol injects → both alice and bob pass.

- [ ] **Step 2: Run, expect pass**

- [ ] **Step 3: Commit**

```
git commit -am "e2e: voice_three_way (full mesh, content checked)"
```

---

### Task 30: `voice_churn.spec.js`

**Files:**
- Create: `web/e2e/voice_churn.spec.js`

- [ ] **Step 1: Write four sub-tests**

- Late joiner: A+B in call, C joins; C receives A's frames within 3s; A's UI shows C in_call within 2s.
- Early leaver: A+B+C in call; C clicks leave; A and B's UIs show C absent within 6s; A injects, B still receives.
- Hard departure: A+B+C in call; C closes its page; A and B see C absent within 6s; assert via test hook `voice_active_peers()` that C is gone.
- Re-join: A+B in call; B clicks leave then join; A's recorder for B shows two epochs of monotonic counters with reset.

- [ ] **Step 2: Run, expect pass**

- [ ] **Step 3: Commit**

```
git commit -am "e2e: voice_churn (late join, early leave, hard departure, rejoin)"
```

---

### Task 31: `voice_mute_deafen.spec.js`

**Files:**
- Create: `web/e2e/voice_mute_deafen.spec.js`

- [ ] **Step 1: Write the tests**

- Alice clicks mic icon → bob sees muted icon within 2s; bob's frame recorder shows alice's frames stop; alice's heartbeats continue (verify via `voice_active_peers()` on bob).
- Alice clicks mic again → frames resume within 2s.
- Alice clicks headphones icon → alice's frame recorder freezes for all peers; alice still sees bob's talking light.
- Alice clicks headphones again → frames resume within 200 ms.
- Per-peer mute-for-me: alice opens bob's popover, toggles mute-for-me; verify GainNode for bob is 0 via `await alice.evaluate(() => __voiceFfi.getPeerGain('<bobhex>'))` (implementer: expose `getPeerGain` from the FFI module on `window` for testing, gated by env).

- [ ] **Step 2: Run, commit**

```
git commit -am "e2e: voice_mute_deafen (mic, deafen, per-peer mute-for-me)"
```

---

### Task 32: `voice_mic_permission.spec.js`

**Files:**
- Create: `web/e2e/voice_mic_permission.spec.js`

- [ ] **Step 1: Write the test**

```js
test("denied microphone surfaces a toast and rolls back", async ({ browser }) => {
  const ctx = await browser.newContext(); // no permission grant
  await ctx.clearPermissions();
  const page = await ctx.newPage();
  await page.goto(`/#voice-test`);

  await page.locator('[data-testid="voice-channel-row"]').click();

  await expect(page.locator('[data-testid="voice-minibar"]')).not.toBeVisible({ timeout: 2000 });
  await expect(page.getByText(/microphone/i)).toBeVisible({ timeout: 2000 });
});

test("granted microphone allows join", async ({ browser }) => {
  const ctx = await browser.newContext({ permissions: ["microphone"] });
  const page = await ctx.newPage();
  await page.goto(`/#voice-test`);
  await page.locator('[data-testid="voice-channel-row"]').click();
  await expect(page.locator('[data-testid="voice-minibar"]')).toBeVisible({ timeout: 500 });
});
```

- [ ] **Step 2: Run, commit**

```
git commit -am "e2e: voice_mic_permission (rollback + toast on deny)"
```

---

### Task 33: `voice_real_mic.spec.js` (Chromium fake-audio-capture)

**Files:**
- Create: `web/e2e/voice_real_mic.spec.js`
- Create: `web/audio/test-fixtures/sweep.wav`
- Create: `web/audio/test-fixtures/README.md`
- Modify: `web/playwright.config.js` (additional project with launch flags)

- [ ] **Step 1: Generate `sweep.wav`**

Use `sox` (added to flake.nix) or a Rust script:

```
nix develop --command sox -n -r 48000 -c 1 web/audio/test-fixtures/sweep.wav synth 5 sine 440
```

Document in README.

- [ ] **Step 2: Add a Playwright project with the fake-audio-capture flag**

```js
{
  name: "chromium-real-mic",
  use: {
    ...devices["Desktop Chrome"],
    permissions: ["microphone"],
    launchOptions: {
      args: [
        "--use-fake-device-for-media-stream",
        "--use-fake-ui-for-media-stream",
        `--use-file-for-fake-audio-capture=${require("path").resolve(__dirname, "audio/test-fixtures/sweep.wav")}`,
      ],
    },
  },
  testMatch: /voice_real_mic\.spec\.js/,
},
```

- [ ] **Step 3: Write the test**

Two browsers, both join voice. Bob asserts within 3s that ≥ 40 frames have been recorded from alice and that alice's `talking` light is on. No content check (it'll be the real wav with browser DSP applied).

- [ ] **Step 4: Run, commit**

```
git commit -am "e2e: voice_real_mic (real worklet path with fake-audio-capture)"
```

---

## Phase 6 — Cleanup

### Task 34: Update existing fixture-backed `voice.spec.js`

**Files:**
- Modify: `web/e2e/voice.spec.js`

- [ ] **Step 1: Decide: keep as smoke or remove**

Recommended: keep as a "popover renders correctly given fixture data" smoke. Update tests that rely on `m.in_call` from fixture to ensure the popover still opens correctly when in_call is now derived from `model.voice.peers`. Replace fixture-only state with a test seed that primes `model.voice.peers` directly (add a `window.__seedVoicePeers([{hex, in_call, talking, is_muted}])` test affordance).

- [ ] **Step 2: Commit**

```
git commit -am "e2e/voice: adapt fixture-backed popover tests for real voice model"
```

---

### Task 35: Final lint, format, full test pass, sanity check

- [ ] **Step 1: Workspace lint + fmt + tests**

```
nix develop --command cargo fmt --all --check
nix develop --command cargo clippy --workspace --all-features --all-targets -- -D warnings
nix develop --command cargo test --workspace --all-features
```

Fix anything that breaks.

- [ ] **Step 2: Build wasm with test-hooks**

```
nix develop --command cargo build -p sunset-web-wasm --target wasm32-unknown-unknown --features test-hooks
```

- [ ] **Step 3: Run all Playwright tests**

```
nix develop --command sh -c "cd web && npx playwright test"
```

Expected: all green.

- [ ] **Step 4: Commit any tidy-up**

```
git commit -am "C2c: final lint, formatting, full test pass"
```

---

## Self-review

**Spec coverage** — each major spec section maps to tasks:

- Wire-format `Heartbeat::is_muted` → Task 1 ✓
- `VoiceRuntime` traits + skeleton → Tasks 2, 3 ✓
- Heartbeat task → Task 4 ✓
- Subscribe loop → Task 6 ✓
- Combiner → Task 7 ✓
- Auto-connect FSM → Task 8 ✓
- Jitter buffer → Task 9 ✓
- Mute/deafen state → Tasks 5, 9 (mute drops frames; deafen skips delivery) ✓
- Drop-cancellation → Task 10 ✓
- Crate split (sunset-voice / sunset-web-wasm) → Phases 1 + 2 ✓
- Per-peer GainNode → Task 17 (browser side); Task 15 (Rust FFI) ✓
- FFI surface (set_muted, set_deafened, set_peer_volume, test-hooks) → Tasks 14, 15, 16 ✓
- Gleam UI wiring (model, click handlers, voice channel, minibar, popover) → Tasks 21, 22, 23, 24, 25 ✓
- Mic permission UX → Task 26 ✓
- Tests: protocol regression (slim) → Task 19; 2-way → Task 28; 3-way → Task 29; churn → Task 30; mute/deafen → Task 31; permission → Task 32; real mic → Task 33 ✓
- Existing voice.spec.js update → Task 34 ✓

**Placeholder scan** — no `TBD`/`TODO` left in steps. Several steps say "Implementer: pick" for narrow micro-decisions (e.g. exact accessor naming, where to put a small helper) — those are intentional latitude, not gaps.

**Type consistency** — `VoicePeerState` fields (`peer`, `in_call`, `talking`, `is_muted`) used consistently across Tasks 2, 7, 13, 21, 25. `Dialer::ensure_direct(PeerId)` consistent across Tasks 2, 8, 12. `FrameSink::deliver(&PeerId, &[f32])` and `drop_peer(&PeerId)` consistent across Tasks 2, 13, 16. JS-side `voice_start(room, on_pcm, on_drop, on_state)` consistent across Tasks 14, 18, 20.

**Risks / things to watch during execution:**

1. The `DynBus` indirection (Task 2/3/11) might run into `Send` issues. The trait is `?Send`; the impl on `BusImpl` must be too. Test compiles before moving on.
2. `voice_start` moving from `Client` → `RoomHandle` (Task 14): this is a breaking FFI change. The harness page (Task 18) and Gleam FFI (Task 20) must align. If keeping `Client.voice_start(room_name)` for FFI parity, the implementer routes to the `RoomHandle` internally — also fine.
3. Per-peer GainNode (Task 15): the test for mute-for-me (Task 31) reads the GainNode `.gain.value` from the FFI module. Make sure the FFI exposes `getPeerGain` for tests (gate behind `globalThis.__expose_voice_internals` set by the test runner).
4. The Lustre `Effect` system needs to forward async results from `voice_start` (Promise) to a `Msg` dispatch. If unfamiliar, look at how `add_relay` / `open_room` are wired today (similar Promise-returning FFI in Gleam).
