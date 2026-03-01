@external(javascript, "./dispatch.ffi.mjs", "set_handler")
pub fn set_handler(_peer_id: String, _handler: fn(String) -> Nil) -> Nil {
  Nil
}

@external(javascript, "./dispatch.ffi.mjs", "remove_handler")
pub fn remove_handler(_peer_id: String) -> Nil {
  Nil
}

@external(javascript, "./dispatch.ffi.mjs", "dispatch")
pub fn dispatch(_peer_id: String, _msg: String) -> Nil {
  Nil
}
