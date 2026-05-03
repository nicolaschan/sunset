# Sunset Voice Network (C2b) Design

**Date:** 2026-05-02
**Scope:** Voice over the network for one peer pair in a single room: encrypted `VoicePacket` wire format, Bus integration, membership + frame liveness, FFI extensions on `Client`. Plus a small `PeerSupervisor::subscribe` addition + connection-liveness FFI so the JS side can render per-peer connection state.
**Out of scope:** multi-peer mixing, jitter / reorder buffer, mute UX, Gleam UI changes (all C2c).

**Builds on:**
- C2a `sunset-voice` codec abstraction (`VoiceEncoder` / `VoiceDecoder` passthrough today)
- C2a audio bridge in `sunset-web-wasm/src/voice.rs` (mic → worklet → wasm; wasm → worklet → speakers)
- Plan A WebRTC unreliable datachannel
- C1 `sunset-core::Liveness`
- `sunset-core::Bus` + `BusImpl`
- `sunset-core::Room` + `crypto::aead`
- `sunset-sync::PeerSupervisor`

## Goal

End-to-end working voice between two browsers in the same sunset.chat room: alice presses a "join voice" button (in a test harness page, not the Gleam UI), bob does the same in another tab/browser, alice talks, bob hears alice within ~80 ms latency. Bob's UI (test harness) shows a per-peer "in-call / talking" indicator backed by `Liveness`. The same UI shows per-peer connection state from `PeerSupervisor`.

## Architecture

```
JS test harness                                                 JS test harness
    |                                                                ^
    | voice_input(pcm)                                                | on_frame(from, pcm)
    v                                                                 |
sunset-web-wasm::voice                                       sunset-web-wasm::voice
    |                                                                 ^
    | encrypt(VoicePacket::Frame{...})                                | decrypt → match
    | Bus::publish_ephemeral(b"voice/<room>/<self>", ct)              | feed Liveness
    v                                                                 | call on_frame
SyncEngine ──► relay (or P2P) ──► remote SyncEngine ────► Bus subscribe(NamePrefix(b"voice/<room>/"))
```

Two parallel timer-driven loops on each side:

1. **Heartbeat loop** (every 2 s while voice_start active): publishes `VoicePacket::Heartbeat` to the same per-sender namespace.
2. **Frame send** (per `voice_input` call from JS, ~50 Hz when actively speaking): publishes `VoicePacket::Frame`.

Receive side has one `Bus::subscribe(NamePrefix(b"voice/<room_fp>/"))` stream that decrypts each packet, dispatches by enum variant:
- `Frame` → feed `frame_liveness` (stale_after=1000ms) + decode PCM + call `on_frame` JS callback
- `Heartbeat` → feed `membership_liveness` (stale_after=5000ms)

State changes from either Liveness arc combine into a `VoicePeerState { in_call: bool, talking: bool }` event delivered via `on_voice_peer_state`.

## Wire format

All in `sunset-voice/src/packet.rs`:

```rust
#[derive(Serialize, Deserialize)]
pub enum VoicePacket {
    Frame { codec_id: String, seq: u64, sender_time_ms: u64, payload: Vec<u8> },
    Heartbeat { sent_at_ms: u64 },
}

#[derive(Serialize, Deserialize)]
pub struct EncryptedVoicePacket {
    pub nonce: [u8; 24],
    pub ciphertext: Vec<u8>,
}
```

postcard-encoded. The `EncryptedVoicePacket` is the byte payload of a `SignedDatagram`.

### Encryption

```rust
pub const VOICE_KEY_DOMAIN: &[u8] = b"sunset/voice/key/v1";
pub const VOICE_AAD_DOMAIN: &[u8] = b"sunset/voice/aad/v1";

pub fn derive_voice_key(room: &Room, epoch_id: u64) -> Result<Zeroizing<[u8; 32]>>;

pub fn encrypt(
    room: &Room,
    epoch_id: u64,
    sender: &IdentityKey,
    packet: &VoicePacket,
    rng: &mut impl CryptoRngCore,
) -> Result<EncryptedVoicePacket>;

pub fn decrypt(
    room: &Room,
    epoch_id: u64,
    sender: &IdentityKey,
    ev: &EncryptedVoicePacket,
) -> Result<VoicePacket>;
```

- `derive_voice_key`: HKDF-SHA256(ikm=epoch_root(epoch_id), info=`VOICE_KEY_DOMAIN || epoch_id.to_le_bytes()`).expand(32). Errors if epoch_id is not present in `Room`.
- `encrypt`: postcard(packet) → AEAD with 24-byte random nonce, AAD = `VOICE_AAD_DOMAIN || room_fp || epoch_id_le || sender.as_bytes()`.
- `decrypt`: reverse; returns `Error::AeadAuthFailed` on tag mismatch. Per the AEAD AAD binding, this fails if the wrong room key is used, if a packet from sender X is replayed claiming to be from sender Y, or if a packet for one epoch is replayed into another.

> **Revision 2026-05-02:** AAD now binds `epoch_id_le` between `room_fp` and `sender.as_bytes()`. This mirrors `sunset-core::crypto::aead::build_msg_aad` which already binds the epoch into the AD; the per-message HKDF already key-separates epochs, so this is belt-and-suspenders against ciphertext substitution across epochs. Frozen vector pinned in `crates/sunset-voice/src/packet.rs::derive_voice_key_frozen_vector`.

`epoch_id = 0` everywhere in v1 (Room rotation isn't in v1). The parameter is plumbed through so future epoch rotation needs only a wire-compatible upgrade, not an API change.

### Authenticity

Outer Ed25519 signature over the `SignedDatagram`'s `(verifying_key, name, payload)` is the sender authentication. AEAD-only authentication would only prove "someone with the room key sent this" — not which member. We must not strip or replace that outer sig.

## Namespaces

One per (sender, room):

- Frames + heartbeats both go to: `voice/<room_fp_hex>/<sender_pubkey_hex>`

(Single namespace for both packet types — the enum tag distinguishes them on receipt. Justification: a subscriber that wants membership always wants frames too in C2b, so namespace splitting saves nothing. C2c can revisit if a UI surface only wants membership.)

Subscribers use `Filter::NamePrefix(b"voice/<room_fp_hex>/")` to receive from all senders.

## Liveness wiring

Two arcs per `VoiceState`:

```rust
let frame_liveness    = Liveness::new(Duration::from_millis(1000));
let membership_liveness = Liveness::new(Duration::from_secs(5));
```

Subscribe loop (single task):

```rust
while let Some(ev) = stream.next().await {
    let BusEvent::Ephemeral(datagram) = ev else { continue; };
    let sender = IdentityKey::from_store_verifying_key(&datagram.verifying_key)?;
    let peer = PeerId(datagram.verifying_key.clone());
    let ev: EncryptedVoicePacket = postcard::from_bytes(&datagram.payload)?;
    let packet = sunset_voice::packet::decrypt(&room, 0, &sender, &ev)?;
    match packet {
        VoicePacket::Frame { sender_time_ms, payload, .. } => {
            frame_liveness.observe(peer.clone(), ms_to_systemtime(sender_time_ms)).await;
            let pcm = decoder.decode(&payload)?;
            on_frame.call2(&JsValue::NULL, &peer_id_uint8array(&peer), &float32_array(&pcm))?;
        }
        VoicePacket::Heartbeat { sent_at_ms } => {
            membership_liveness.observe(peer, ms_to_systemtime(sent_at_ms)).await;
        }
    }
}
```

State combiner task:

```rust
// Watches both Liveness streams, emits combined VoicePeerState to JS callback.
let mut frame_sub      = frame_liveness.subscribe().await;
let mut membership_sub = membership_liveness.subscribe().await;
let mut talking = HashMap::<PeerId, bool>::new();
let mut in_call = HashMap::<PeerId, bool>::new();
loop {
    select! {
        Some(ev) = frame_sub.next()      => { talking.insert(ev.peer.clone(), ev.state == LivenessState::Live); emit(...); }
        Some(ev) = membership_sub.next() => { in_call.insert(ev.peer.clone(), ev.state == LivenessState::Live); emit(...); }
    }
}
```

`emit` calls `on_voice_peer_state.call3(NULL, peer_id_uint8array, in_call, talking)`.

`in_call` is `OR(membership_live, frame_live)` — a peer that's actively talking is implicitly in_call even if their heartbeat happens to be late. `talking` is just `frame_live`. If neither map has seen the peer yet, both default to `false`. The combiner only emits when at least one bool changes vs the last emitted state for that peer (debounce).

## FFI surface

### Existing C2a methods (extended, loopback removed)

```rust
#[wasm_bindgen]
impl Client {
    pub fn voice_start(
        &self,
        on_frame: &js_sys::Function,
        on_voice_peer_state: &js_sys::Function,
    ) -> Result<(), JsError>;

    pub fn voice_stop(&self) -> Result<(), JsError>;

    pub fn voice_input(&self, pcm: &js_sys::Float32Array) -> Result<(), JsError>;
}
```

- `voice_start`: returns `Err(JsError::new("voice already started"))` on second call before `voice_stop` (surfaces harness programming bugs). Spawns: subscribe loop, heartbeat timer, state combiner. Constructs encoder + decoder (single instance each — passthrough is stateless, future codecs may not be).
- `voice_stop`: drops `VoiceState` (cancels everything via Drop on the Rc); receive callbacks stop firing within one loop iteration.
- `voice_input`: encrypts a `VoicePacket::Frame` with monotonically increasing `seq`, current `sender_time_ms`, calls `Bus::publish_ephemeral`.

`on_frame(from_peer_id: Uint8Array, pcm: Float32Array)`. `on_voice_peer_state(peer_id: Uint8Array, in_call: bool, talking: bool)`.

### New connection-liveness methods

```rust
#[wasm_bindgen]
impl Client {
    pub fn on_peer_connection_state(&self, handler: &js_sys::Function) -> Result<(), JsError>;
    pub fn peer_connection_snapshot(&self) -> Result<JsValue, JsError>;
}
```

Backed by `PeerSupervisor`. Snapshot serializes to a `Vec<{ addr: string, state: string, peer_id: Option<Vec<u8>>, attempt: u32 }>` via `serde_wasm_bindgen`. Handler invoked with the same shape per state transition.

### `sunset-sync::PeerSupervisor` extension

Add:

```rust
impl<S, T> PeerSupervisor<S, T> {
    pub fn subscribe(&self) -> LocalBoxStream<'static, IntentSnapshot>;
}
```

Internally: an `mpsc::UnboundedSender<IntentSnapshot>` list under the same `Mutex` that protects intent state mutation. Every state transition (`Connecting` → `Connected`, etc.) broadcasts to all live subscribers. Same idiom as `Liveness::subscribe`.

This is the minimum change to `sunset-sync` for C2b. Cleaner than polling `snapshot()` from JS.

## Test plan

All five tests must pass before C2b is considered done.

1. **`sunset-voice` unit tests** (`crates/sunset-voice/src/packet.rs`, host target):
   - `encrypt_decrypt_round_trip_frame`
   - `encrypt_decrypt_round_trip_heartbeat`
   - `decrypt_wrong_room_fails` — open two different `Room`s, encrypt with one, decrypt with other → `AeadAuthFailed`
   - `decrypt_wrong_sender_fails` — AAD binds sender; tampered sender field → `AeadAuthFailed`
   - `decrypt_tampered_ciphertext_fails` — flip one byte in `ciphertext`

2. **`sunset-core` integration test** (`crates/sunset-core/tests/voice_two_peer.rs`, host target):
   - Spin up 2 `BusImpl<MemoryStore, TestTransport>` over a `TestNetwork`.
   - Both subscribe with `Filter::NamePrefix(b"voice/<room_fp_hex>/")`.
   - Alice publishes an encrypted `VoicePacket::Frame`.
   - Assert bob receives a `BusEvent::Ephemeral`, decrypts to the same `VoicePacket::Frame` byte-for-byte.
   - Assert bob's `frame_liveness` transitions to `Live` after observe.

3. **`wasm-bindgen-test` for `sunset-web-wasm::voice`** (target wasm):
   - `voice_start_then_stop_does_not_leak` (sanity).
   - `voice_input_publishes_frame_via_bus` — drive Bus through a mock transport, assert the published name matches `voice/<room_fp_hex>/<self>` and the decrypted payload deserialises to a `Frame`.
   - `subscriber_dispatches_to_on_frame_for_frame_packet` — synthesize a `BusEvent::Ephemeral` upstream, verify `on_frame` fires once with the expected pcm bytes.

4. **Playwright e2e — basic round-trip** (`web/playwright/voice-roundtrip.spec.ts`):
   - Set up: spawn `sunset-relay` via Nix flake app, two chromium pages each load `web/voice-e2e-test.html` (a tiny harness that exposes `Client.voice_start` / `voice_input` / awaits `on_frame` on `window`).
   - Both join the same room with the same relay.
   - Wait for `peer_connection_state` on both sides to show the other as `Connected`.
   - Alice calls `voice_input` with a known synthetic PCM frame (e.g. 960 samples of a sine).
   - Bob's `on_frame` callback fires within 500 ms; PCM bytes are byte-equal to alice's input (passthrough codec is bit-exact).
   - **No production code reads the test page.** All FFI used is the same as the Gleam UI will use in C2c.

5. **Playwright e2e — voice peer state transitions** (same spec file, separate test case):
   - After alice's `voice_start`, bob's `on_voice_peer_state` fires with `(alice, in_call=true, talking=false)` within 2.5 s (one heartbeat interval).
   - After alice's first `voice_input`, bob's `on_voice_peer_state` fires with `(alice, in_call=true, talking=true)` within 200 ms.
   - After alice stops calling `voice_input` for >1 s, bob sees `talking=false`.
   - After alice's `voice_stop` and 5 s of no heartbeats, bob sees `in_call=false`.

The Playwright spec runs under `nix develop --command npx playwright test`. Playwright + chromium go in `flake.nix` as packages; no global install.

## File touchpoints

New files:
- `crates/sunset-voice/src/packet.rs`
- `crates/sunset-web-wasm/src/voice/mod.rs` (replaces existing `voice.rs`)
- `crates/sunset-web-wasm/src/voice/transport.rs`
- `crates/sunset-web-wasm/src/voice/subscriber.rs`
- `crates/sunset-web-wasm/src/voice/liveness.rs`
- `crates/sunset-core/tests/voice_two_peer.rs`
- `web/voice-e2e-test.html`
- `web/playwright/voice-roundtrip.spec.ts`
- `web/playwright/playwright.config.ts`

Modified:
- `crates/sunset-voice/src/lib.rs` — `pub mod packet;`
- `crates/sunset-voice/Cargo.toml` — add `bytes`, `postcard`, `rand_core`, `serde`, `sunset-core`, `sunset-store`, `zeroize` (currently only `thiserror`).
- `crates/sunset-sync/src/supervisor.rs` — add `subscribe()`
- `crates/sunset-web-wasm/src/lib.rs` — `mod voice;` becomes `mod voice;` (path-based, same name, just split internally)
- `crates/sunset-web-wasm/src/client.rs` — extend `voice_start` signature, drop loopback wiring; add `on_peer_connection_state` + `peer_connection_snapshot`
- `flake.nix` — add `nodejs`, `playwright-driver` (or `playwright-test` package), chromium browser

Removed:
- `web/voice-demo.html` — loopback demo, replaced by `voice-e2e-test.html`
- `crates/sunset-web-wasm/src/voice.rs` — split into `voice/`

## Open issues / non-goals

- **Jitter buffer / reorder buffer** — C2c. C2b plays frames in arrival order. Single-peer over a clean LAN connection (which the Playwright test uses) won't visibly need it.
- **Mute UX** — C2c. C2b has no Mute / Unmute on the wire; the enum has room for it.
- **Multi-peer mixing** — C2c. The receive code already handles multiple sources (separate `seq` per peer, separate Liveness entries) — what's missing is a downstream PCM mixer before playback. The Playwright test in C2b is single-peer; the wasm code crashes-free with two senders is a deferred verification.
- **Codec swap** — `codec_id` field is on the wire from day one so a future codec doesn't require a wire bump. The codec itself is still the C2a passthrough; revisit per `2026-04-30-sunset-voice-codec-decision.md`.
- **Gleam UI integration** — C2c. The voice FFI is callable from Gleam today; nobody calls it. The Playwright test bypasses the Gleam UI by loading `voice-e2e-test.html` directly.
- **Voice membership across rooms** — out of scope. v1 only supports voice in the room the Client was constructed with.
