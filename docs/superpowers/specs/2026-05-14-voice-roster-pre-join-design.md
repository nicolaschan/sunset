# Voice channel roster visible before join

## Problem

Today, the channel rail's voice block only renders a live roster after the
local user joins the voice call. The mechanism: clicking *Join* calls
`voice_start`, which constructs `VoiceRuntime`, which spawns the
`voice_presence_membership` task. Until that task runs, the client never
subscribes to the durable `voice-presence/<room_fp>/<peer>` entries, so no
peer-state events reach the UI. The channel rail therefore falls back to
the *idle* shape (`c.in_call == 0` ⇒ `idle_voice_row`).

This is a missing-data problem, not a rendering one: `live_voice_block`,
`voice_member_row`, and the `in_voice_channel` flag in `VoicePeerStateUI`
already handle the "in channel, not connected yet" case correctly — they
just never receive the data because the subscription isn't running.

The fix is to observe voice-channel presence as soon as the user enters a
room, not when they join the call.

## Approach

Split the voice subsystem's startup into two phases:

1. **Observe** — bus subscription to durable voice-presence entries.
   No mic, no audio context, no WebRTC dial, no presence publishing.
   Runs whenever the local user is in the room.
2. **Activate** — mic capture + audio worklet + WebRTC dial +
   presence publishing + heartbeats. Runs whenever the local user is
   in the call.

The observe phase emits the same `VoicePeerState` events the UI already
consumes, with `in_call=false, in_voice_channel=true` for peers who have
published voice-presence but aren't yet on a P2P leg.

### Why split the runtime, not duplicate the subscription

A separate "channel presence observer" that lives outside `sunset-voice`
would duplicate the `voice-presence/...` subscription, the
`Liveness`-based staleness tracking, and the prefix-name construction.
The voice runtime already encodes all of that correctly. Splitting
startup into observe/activate phases keeps presence logic in one place
and matches the lifecycle that already exists implicitly (presence is a
room-scoped concept; mic capture is a call-scoped concept).

### Why not just always start the full runtime at room load

Two reasons:

- **Mic permission.** `startCapture` calls `getUserMedia`, which prompts
  the user for mic access. Triggering that prompt on room entry, before
  the user has expressed any intent to join voice, is a UX regression.
- **Bandwidth and battery.** The full runtime publishes durable
  presence every 2 s, sends ephemeral heartbeats, and runs the
  auto-connect FSM. None of that should run for a user who is only
  observing the channel.

## Design

### `sunset-voice` changes

Add an `is_active` flag to `RuntimeInner`, default `false`. The three
tasks that emit data (publisher, heartbeat, auto_connect) consult this
flag at the top of each loop iteration and skip work when inactive. The
three observe-side tasks (subscribe, combiner, voice_presence_membership)
run unconditionally — they only *consume* events, so leaving them on
costs only the bus subscription itself.

Add `VoiceRuntime::set_active(bool)` to flip the flag. Activation is
idempotent.

Construction (`VoiceRuntime::new`) is unchanged in signature, but the
default `is_active` is `false`. Callers who want immediate activation
call `set_active(true)` right after `new`.

### `sunset-web-wasm` changes

Split the WASM-facing API:

- `voice_observe_start(cell, identity, room_handle, bus, on_pcm,
  on_drop_peer, on_voice_peer_state)` — constructs the runtime with
  `is_active=false` and spawns all six tasks. Returns immediately; no
  JS-side audio touched. `on_pcm` / `on_drop_peer` are passed through
  so the same callbacks survive the eventual `activate`, but they
  cannot fire pre-activation (no frames arrive without P2P).
- `voice_activate(cell)` — sets `is_active=true`. JS side must have
  already called `startCapture` before invoking.
- `voice_deactivate(cell)` — sets `is_active=false`. JS side stops mic
  capture. Observer subscription stays up. Peer states emitted from
  this point forward will have `in_call=false` until peers republish
  presence (TTL expires within `VOICE_PRESENCE_STALE_AFTER`).
- `voice_stop(cell)` — drops the runtime entirely. Used when the user
  leaves the room. Existing semantics preserved.

`voice_start` (the existing one-shot) remains as a convenience that
calls observe_start + activate, so existing tests / callers that
combine join + capture in one step still work.

### JS FFI changes

Add `wasmVoiceObserveStart(client, roomHandle, callback)`: does *not*
call `startCapture`. Otherwise mirrors `wasmVoiceStart` (wires the same
three callbacks).

Add `wasmVoiceActivate(client, callback)`: calls `startCapture` and
then `client.voice_activate()`. The user-quality-preset re-apply that
currently lives in `wasmVoiceStart` moves here, since it depends on
the encoder being meaningfully used.

Add `wasmVoiceDeactivate(client)`: calls `client.voice_deactivate()`
and `stopCapture()`. Leaves the observer running.

`wasmVoiceStop` is unchanged (full teardown for room exit).

### Gleam layer changes

In `sunset_web.gleam`:

- After the room handle becomes available (in the existing
  `RoomReady` / equivalent handler that today does nothing for voice),
  dispatch an effect that calls `voice.voice_observe_start`. Pass the
  same three callbacks already used by `voice_start`: pcm, drop_peer,
  and voice peer state.
- On `JoinVoice`, switch from `voice.voice_start` to
  `voice.voice_activate` (assumes observer already running).
- On `LeaveVoice`, switch from `voice.voice_stop` to
  `voice.voice_deactivate`. Keep the model's `self_in_call`/`peers`
  reset behaviour intact.
- On leaving a room (room switch or disconnect), call
  `voice.voice_stop` to fully tear down the runtime, and re-observe
  in the new room.

The reducer for `VoicePeerStateChanged` is unchanged: it already
handles `in_voice_channel=true && in_call=false` correctly.

The channels view already routes via `c.in_call > 0` where
`c.in_call = live_voice_count = count of members where m.in_call =
in_voice_channel_now`. Once the pre-join observer fires
`VoicePeerStateChanged` events for peers with `in_voice_channel=true`,
the rail will automatically switch to `live_voice_block` and render
peers with `"connecting…"` labels, without any view-layer change.

### Error handling

`voice_observe_start` can fail if the bus subscription fails. This is
non-fatal: the rail simply stays in the idle shape, same as today's
behaviour for users who never join. Log a warning, don't surface a
toast — the user isn't actively waiting for voice-channel feedback.

`voice_activate` failure surfaces the existing `VoicePermissionDenied`
toast path (the only realistic failure is `getUserMedia` rejection).

## Testing

Honest e2e (Playwright):

- `voice_channel_roster_pre_join.spec.js`: two peers in a room.
  - Peer A clicks Join. Verify Peer A's UI shows the live block with
    themselves in the roster.
  - Peer B does *not* click Join. Verify Peer B's UI also shows the
    live block, with Peer A in the roster labelled "connecting…"
    (because Peer B has no P2P connection to Peer A) and Peer B
    *not* in the roster (Peer B hasn't joined).
  - Verify Peer B's toggle button reads "Join general".
  - Peer A clicks Leave. Verify Peer B's rail returns to the idle
    shape within `VOICE_PRESENCE_STALE_AFTER` (~10 s budget).
  - Test must use real durable presence flow over a relay, not
    mocks. Timeouts reflect UX: TTL+sweep should resolve within ~10 s,
    so we wait up to 12 s for the transition.

Rust integration test (`crates/sunset-voice/tests/`):
- New test: `observe_only_emits_in_voice_channel`. Construct a runtime
  with `is_active=false`. Have a peer publish durable voice-presence
  on the bus. Assert the peer-state sink receives an emission with
  `in_call=false, in_voice_channel=true`. Then `set_active(true)`,
  assert publisher starts emitting durable presence for self.

Unit tests in `sunset-voice/src/runtime/voice_presence_publisher.rs`
and `heartbeat.rs`: assert the loop bails when `is_active=false`.

## Out of scope

- No new UI affordances. The existing live block and "connecting…"
  label cover the pre-join case.
- No "preview voice" or eavesdrop mechanism. We do not subscribe to
  ephemeral voice frames pre-activation; the user must explicitly
  join to hear audio. Only durable presence is observed.
- No changes to mute/deafen UI in the idle voice block. The minibar
  appears only when self is in call (`self_in_call`), unchanged.
- Bridges (which surface as `Bridge` channel kind) are unaffected.
