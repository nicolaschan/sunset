@external(javascript, "./libp2p.ffi.mjs", "init_libp2p")
pub fn init_libp2p(_dispatch: fn(String) -> Nil) -> Nil {
  Nil
}

@external(javascript, "./libp2p.ffi.mjs", "dial_multiaddr")
pub fn dial_multiaddr(
  _addr: String,
  _on_ok: fn() -> Nil,
  _on_error: fn(String) -> Nil,
) -> Nil {
  Nil
}

@external(javascript, "./libp2p.ffi.mjs", "get_multiaddrs")
pub fn get_multiaddrs() -> List(String) {
  []
}

@external(javascript, "./libp2p.ffi.mjs", "get_connected_peers")
pub fn get_connected_peers() -> List(String) {
  []
}

@external(javascript, "./libp2p.ffi.mjs", "get_connection_count")
pub fn get_connection_count() -> Int {
  0
}

@external(javascript, "./libp2p.ffi.mjs", "set_timeout")
pub fn set_timeout(_callback: fn() -> Nil, _ms: Int) -> Nil {
  Nil
}

@external(javascript, "./libp2p.ffi.mjs", "register_chat_handler")
pub fn register_chat_handler(_on_message: fn(String, String) -> Nil) -> Nil {
  Nil
}

@external(javascript, "./libp2p.ffi.mjs", "broadcast_message")
pub fn broadcast_message(
  _text: String,
  _on_ok: fn() -> Nil,
  _on_error: fn(String) -> Nil,
) -> Nil {
  Nil
}

@external(javascript, "./libp2p.ffi.mjs", "start_audio")
pub fn start_audio(_on_ok: fn() -> Nil, _on_error: fn(String) -> Nil) -> Nil {
  Nil
}

@external(javascript, "./libp2p.ffi.mjs", "stop_audio")
pub fn stop_audio() -> Nil {
  Nil
}

@external(javascript, "./libp2p.ffi.mjs", "close_all_audio_pcs")
pub fn close_all_audio_pcs() -> Nil {
  Nil
}

@external(javascript, "./libp2p.ffi.mjs", "is_audio_active")
pub fn is_audio_active() -> Bool {
  False
}

@external(javascript, "./libp2p.ffi.mjs", "is_receiving_audio")
pub fn is_receiving_audio() -> Bool {
  False
}

@external(javascript, "./libp2p.ffi.mjs", "join_audio_listening")
pub fn join_audio_listening() -> Nil {
  Nil
}

@external(javascript, "./libp2p.ffi.mjs", "leave_audio_listening")
pub fn leave_audio_listening() -> Nil {
  Nil
}

@external(javascript, "./libp2p.ffi.mjs", "is_audio_joined")
pub fn is_audio_joined() -> Bool {
  False
}

@external(javascript, "./libp2p.ffi.mjs", "subscribe_to_room")
pub fn subscribe_to_room(
  _room: String,
  _on_peer_discovered: fn(String, List(String)) -> Nil,
) -> Nil {
  Nil
}

@external(javascript, "./libp2p.ffi.mjs", "unsubscribe_from_room")
pub fn unsubscribe_from_room() -> Nil {
  Nil
}

@external(javascript, "./libp2p.ffi.mjs", "get_relay_peer_id")
pub fn get_relay_peer_id() -> String {
  ""
}

@external(javascript, "./libp2p.ffi.mjs", "get_peer_remote_addrs")
pub fn get_peer_remote_addrs() -> List(List(String)) {
  []
}

@external(javascript, "./libp2p.ffi.mjs", "get_peer_addrs")
pub fn get_peer_addrs(_peer_id: String) -> List(List(String)) {
  []
}

@external(javascript, "./libp2p.ffi.mjs", "dial_peer_addrs")
pub fn dial_peer_addrs(
  _addrs: List(String),
  _on_ok: fn() -> Nil,
  _on_error: fn(String) -> Nil,
) -> Nil {
  Nil
}

@external(javascript, "./libp2p.ffi.mjs", "register_audio_presence_handler")
pub fn register_audio_presence_handler() -> Nil {
  Nil
}

@external(javascript, "./libp2p.ffi.mjs", "register_audio_signaling_handler")
pub fn register_audio_signaling_handler() -> Nil {
  Nil
}

@external(javascript, "./libp2p.ffi.mjs", "broadcast_audio_presence")
pub fn broadcast_audio_presence() -> Nil {
  Nil
}

@external(javascript, "./libp2p.ffi.mjs", "get_peer_audio_states")
pub fn get_peer_audio_states() -> List(List(String)) {
  []
}

@external(javascript, "./libp2p.ffi.mjs", "get_audio_pc_states")
pub fn get_audio_pc_states() -> List(List(String)) {
  []
}

@external(javascript, "./libp2p.ffi.mjs", "get_recently_disconnected_peers")
pub fn get_recently_disconnected_peers() -> List(String) {
  []
}
