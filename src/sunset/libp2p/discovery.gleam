@external(javascript, "./discovery.ffi.mjs", "poll_discovery")
pub fn poll_discovery(
  _relay_peer_id: String,
  _room: String,
  _on_peer: fn(String, List(String)) -> Nil,
) -> Nil {
  Nil
}
