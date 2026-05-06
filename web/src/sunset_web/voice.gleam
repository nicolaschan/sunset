//// FFI bindings for the voice subsystem. Wraps the JS-side
//// voice.ffi.mjs and the wasm Client voice methods.
////
//// `voice_start` calls `getUserMedia` asynchronously then invokes `client.voice_start`.
//// The JS side resolves the Promise internally and calls `callback` with
//// `Ok(Nil)` or `Error(message)` — matching the callback pattern used by
//// the rest of the bridge (no `gleam_javascript` dependency required).

import sunset_web/sunset.{type ClientHandle, type RoomHandle}

@external(javascript, "./voice.ffi.mjs", "ensureCtx")
pub fn ensure_audio_context() -> Nil

@external(javascript, "./voice.ffi.mjs", "stopCapture")
pub fn stop_capture() -> Nil

@external(javascript, "./voice.ffi.mjs", "setPeerVolume")
pub fn set_peer_volume(peer_hex: String, gain: Float) -> Nil

/// Start voice for the given room. Calls `getUserMedia` then wires the capture
/// worklet to the wasm voice runtime. On mic-permission denial, `callback`
/// receives `Error(message)`; on success `Ok(Nil)`.
@external(javascript, "./voice.ffi.mjs", "wasmVoiceStart")
pub fn voice_start(
  client: ClientHandle,
  room_handle: RoomHandle,
  callback: fn(Result(Nil, String)) -> Nil,
) -> Nil

@external(javascript, "./voice.ffi.mjs", "wasmVoiceStop")
pub fn voice_stop(client: ClientHandle) -> Nil

@external(javascript, "./voice.ffi.mjs", "wasmVoiceSetMuted")
pub fn voice_set_muted(client: ClientHandle, muted: Bool) -> Nil

@external(javascript, "./voice.ffi.mjs", "wasmVoiceSetDeafened")
pub fn voice_set_deafened(client: ClientHandle, deafened: Bool) -> Nil

/// Switch the active send-side voice quality preset. Persists the
/// label to localStorage and (if voice is running) pushes the change
/// down to the active encoder. Accepted labels: `"voice"`, `"high"`,
/// `"maximum"`. Unknown labels are silently ignored.
@external(javascript, "./voice.ffi.mjs", "wasmVoiceSetQuality")
pub fn voice_set_quality(client: ClientHandle, label: String) -> Nil

/// Read the persisted quality preset, or the default (`"maximum"`)
/// if nothing has been saved.
@external(javascript, "./voice.ffi.mjs", "wasmVoiceGetQuality")
pub fn voice_get_quality() -> String

/// Install the global `window.__voicePeerStateHandler` callback so
/// `wasmVoiceStart`'s `on_voice_peer_state` fires into Lustre dispatch.
/// Call once at app init via `effect.from`.
@external(javascript, "./voice.ffi.mjs", "installVoiceStateHandler")
pub fn install_voice_state_handler(
  cb: fn(String, Bool, Bool, Bool) -> Nil,
) -> Nil

/// Install the global `window.__voicePeerLevelHandler` callback so
/// per-peer audio-level updates fire into Lustre dispatch. The level is
/// a smoothed 0..1 value computed from the RMS of each delivered PCM
/// frame; the rail's waveform reads from it to reflect who is talking.
/// Call once at app init via `effect.from`.
@external(javascript, "./voice.ffi.mjs", "installVoicePeerLevelHandler")
pub fn install_voice_peer_level_handler(cb: fn(String, Float) -> Nil) -> Nil

/// Install the global `window.__voiceSelfLevelHandler` callback so the
/// local mic level fires into Lustre dispatch. Drives the self row's
/// waveform.
@external(javascript, "./voice.ffi.mjs", "installVoiceSelfLevelHandler")
pub fn install_voice_self_level_handler(cb: fn(Float) -> Nil) -> Nil
