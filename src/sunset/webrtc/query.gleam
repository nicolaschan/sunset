import sunset/webrtc/pc.{type PeerConnection}

@external(javascript, "./query.ffi.mjs", "get_connection_state")
pub fn get_connection_state(_pc: PeerConnection) -> String {
  "closed"
}

@external(javascript, "./query.ffi.mjs", "get_local_description")
pub fn get_local_description(_pc: PeerConnection) -> String {
  ""
}

@external(javascript, "./query.ffi.mjs", "get_local_description_type")
pub fn get_local_description_type(_pc: PeerConnection) -> String {
  ""
}

@external(javascript, "./query.ffi.mjs", "get_signaling_state")
pub fn get_signaling_state(_pc: PeerConnection) -> String {
  "closed"
}
