// ── Timers ──────────────────────────────────────────────────────────

@external(javascript, "./libp2p.ffi.mjs", "set_timeout")
pub fn set_timeout(_callback: fn() -> Nil, _ms: Int) -> Nil {
  Nil
}

// ── libp2p lifecycle ────────────────────────────────────────────────

@external(javascript, "./libp2p.ffi.mjs", "init_libp2p")
pub fn init_libp2p(
  _on_ready: fn(String) -> Nil,
  _on_peer_connect: fn(String) -> Nil,
  _on_peer_disconnect: fn(String) -> Nil,
) -> Nil {
  Nil
}

@external(javascript, "./libp2p.ffi.mjs", "get_local_peer_id")
pub fn get_local_peer_id() -> String {
  ""
}

// ── Dialling ────────────────────────────────────────────────────────

@external(javascript, "./libp2p.ffi.mjs", "dial_multiaddr")
pub fn dial_multiaddr(
  _addr: String,
  _on_ok: fn() -> Nil,
  _on_error: fn(String) -> Nil,
) -> Nil {
  Nil
}

// ── Queries ─────────────────────────────────────────────────────────

@external(javascript, "./libp2p.ffi.mjs", "get_multiaddrs")
pub fn get_multiaddrs() -> List(String) {
  []
}

@external(javascript, "./libp2p.ffi.mjs", "get_connected_peers")
pub fn get_connected_peers() -> List(String) {
  []
}

/// Returns all connections as List(List(String)) where each inner list
/// is [peer_id, remote_addr_string].
@external(javascript, "./libp2p.ffi.mjs", "get_all_connections")
pub fn get_all_connections() -> List(List(String)) {
  []
}

// ── Protocol messaging ──────────────────────────────────────────────

@external(javascript, "./libp2p.ffi.mjs", "register_protocol_handler")
pub fn register_protocol_handler(
  _protocol: String,
  _on_message: fn(String, String) -> Nil,
) -> Nil {
  Nil
}

@external(javascript, "./libp2p.ffi.mjs", "send_protocol_message")
pub fn send_protocol_message(
  _peer_id: String,
  _protocol: String,
  _message: String,
  _on_ok: fn() -> Nil,
  _on_error: fn(String) -> Nil,
) -> Nil {
  Nil
}

@external(javascript, "./libp2p.ffi.mjs", "send_protocol_message_fire")
pub fn send_protocol_message_fire(
  _peer_id: String,
  _protocol: String,
  _message: String,
) -> Nil {
  Nil
}

// ── Audio: microphone ───────────────────────────────────────────────

@external(javascript, "./libp2p.ffi.mjs", "acquire_microphone")
pub fn acquire_microphone(
  _on_ok: fn() -> Nil,
  _on_error: fn(String) -> Nil,
) -> Nil {
  Nil
}

@external(javascript, "./libp2p.ffi.mjs", "mute_microphone")
pub fn mute_microphone() -> Nil {
  Nil
}

@external(javascript, "./libp2p.ffi.mjs", "release_microphone")
pub fn release_microphone() -> Nil {
  Nil
}

@external(javascript, "./libp2p.ffi.mjs", "is_microphone_active")
pub fn is_microphone_active() -> Bool {
  False
}

@external(javascript, "./libp2p.ffi.mjs", "has_microphone")
pub fn has_microphone() -> Bool {
  False
}

// ── Audio: remote playback ──────────────────────────────────────────

@external(javascript, "./libp2p.ffi.mjs", "unmute_remote_audio")
pub fn unmute_remote_audio() -> Nil {
  Nil
}

@external(javascript, "./libp2p.ffi.mjs", "mute_remote_audio")
pub fn mute_remote_audio() -> Nil {
  Nil
}

@external(javascript, "./libp2p.ffi.mjs", "is_receiving_audio")
pub fn is_receiving_audio() -> Bool {
  False
}

// ── Audio: WebRTC peer connections ──────────────────────────────────

@external(javascript, "./libp2p.ffi.mjs", "create_audio_pc")
pub fn create_audio_pc(
  _peer_id: String,
  _should_offer: Bool,
  _audio_muted: Bool,
  _on_state_change: fn(String, String) -> Nil,
) -> Nil {
  Nil
}

@external(javascript, "./libp2p.ffi.mjs", "close_audio_pc")
pub fn close_audio_pc(_peer_id: String) -> Nil {
  Nil
}

@external(javascript, "./libp2p.ffi.mjs", "send_audio_bye")
pub fn send_audio_bye(_peer_id: String) -> Nil {
  Nil
}

@external(javascript, "./libp2p.ffi.mjs", "close_all_audio_pcs")
pub fn close_all_audio_pcs() -> Nil {
  Nil
}

@external(javascript, "./libp2p.ffi.mjs", "register_audio_signaling_handler")
pub fn register_audio_signaling_handler(
  _audio_muted: Bool,
  _on_state_change: fn(String, String) -> Nil,
  _on_bye: fn(String) -> Nil,
) -> Nil {
  Nil
}

@external(javascript, "./libp2p.ffi.mjs", "get_audio_pc_states")
pub fn get_audio_pc_states() -> List(List(String)) {
  []
}

@external(javascript, "./libp2p.ffi.mjs", "has_audio_pc")
pub fn has_audio_pc(_peer_id: String) -> Bool {
  False
}

// ── Discovery ───────────────────────────────────────────────────────

@external(javascript, "./libp2p.ffi.mjs", "poll_discovery")
pub fn poll_discovery(
  _relay_peer_id: String,
  _room: String,
  _on_response: fn(String) -> Nil,
) -> Nil {
  Nil
}
