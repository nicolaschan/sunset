@external(javascript, "./query.ffi.mjs", "get_multiaddrs")
pub fn get_multiaddrs() -> List(String) {
  []
}

@external(javascript, "./query.ffi.mjs", "get_connected_peers")
pub fn get_connected_peers() -> List(String) {
  []
}

/// Returns all connections as List(List(String)) where each inner list
/// is [peer_id, remote_addr_string].
@external(javascript, "./query.ffi.mjs", "get_all_connections")
pub fn get_all_connections() -> List(List(String)) {
  []
}
