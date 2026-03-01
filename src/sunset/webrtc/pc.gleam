pub type PeerConnection

pub type Track

pub type Stream

pub type Sender

pub type PlaybackHandle

@external(javascript, "./pc.ffi.mjs", "create_pc")
pub fn create_pc(
  _on_ice_candidate: fn(String) -> Nil,
  _on_state_change: fn(String) -> Nil,
  _on_negotiation_needed: fn() -> Nil,
  _on_track: fn(Track, Stream) -> Nil,
) -> PeerConnection {
  panic as "create_pc is only available on JavaScript"
}

@external(javascript, "./pc.ffi.mjs", "close_pc")
pub fn close_pc(_pc: PeerConnection) -> Nil {
  Nil
}

@external(javascript, "./pc.ffi.mjs", "wait_for_ice_gathering")
pub fn wait_for_ice_gathering(
  _pc: PeerConnection,
  _timeout_ms: Int,
  _callback: fn() -> Nil,
) -> Nil {
  Nil
}
