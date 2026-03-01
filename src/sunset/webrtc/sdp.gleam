import sunset/webrtc/pc.{type PeerConnection}

@external(javascript, "./sdp.ffi.mjs", "create_offer")
pub fn create_offer(
  _pc: PeerConnection,
  _callback: fn(Result(String, String)) -> Nil,
) -> Nil {
  Nil
}

@external(javascript, "./sdp.ffi.mjs", "create_answer")
pub fn create_answer(
  _pc: PeerConnection,
  _callback: fn(Result(String, String)) -> Nil,
) -> Nil {
  Nil
}

@external(javascript, "./sdp.ffi.mjs", "set_remote_description")
pub fn set_remote_description(
  _pc: PeerConnection,
  _sdp_json: String,
  _callback: fn(Result(Nil, String)) -> Nil,
) -> Nil {
  Nil
}

@external(javascript, "./sdp.ffi.mjs", "add_ice_candidate")
pub fn add_ice_candidate(
  _pc: PeerConnection,
  _candidate_json: String,
  _callback: fn(Result(Nil, String)) -> Nil,
) -> Nil {
  Nil
}
