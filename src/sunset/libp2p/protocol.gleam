@external(javascript, "./protocol.ffi.mjs", "register_protocol_handler")
pub fn register_protocol_handler(
  _protocol: String,
  _on_message: fn(String, String) -> Nil,
) -> Nil {
  Nil
}

@external(javascript, "./protocol.ffi.mjs", "send_protocol_message")
pub fn send_protocol_message(
  _peer_id: String,
  _protocol: String,
  _message: String,
  _callback: fn(Result(Nil, String)) -> Nil,
) -> Nil {
  Nil
}

@external(javascript, "./protocol.ffi.mjs", "send_protocol_message_fire")
pub fn send_protocol_message_fire(
  _peer_id: String,
  _protocol: String,
  _message: String,
) -> Nil {
  Nil
}
