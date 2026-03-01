@external(javascript, "./node.ffi.mjs", "init_libp2p")
pub fn init_libp2p(
  _on_ready: fn(String) -> Nil,
  _on_peer_connect: fn(String) -> Nil,
  _on_peer_disconnect: fn(String) -> Nil,
) -> Nil {
  Nil
}

@external(javascript, "./node.ffi.mjs", "get_local_peer_id")
pub fn get_local_peer_id() -> String {
  ""
}
