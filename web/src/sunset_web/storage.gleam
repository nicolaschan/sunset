//// FFI bindings around localStorage + URL-hash routing.

@external(javascript, "./storage.ffi.mjs", "readJoinedRooms")
pub fn read_joined_rooms() -> List(String)

@external(javascript, "./storage.ffi.mjs", "writeJoinedRooms")
pub fn write_joined_rooms(rooms: List(String)) -> Nil

@external(javascript, "./storage.ffi.mjs", "readLastUsed")
pub fn read_last_used() -> String

@external(javascript, "./storage.ffi.mjs", "writeLastUsed")
pub fn write_last_used(name: String) -> Nil

@external(javascript, "./storage.ffi.mjs", "readHash")
pub fn read_hash() -> String

@external(javascript, "./storage.ffi.mjs", "setHash")
pub fn set_hash(name: String) -> Nil

/// Register a callback that fires every time the URL hash changes.
@external(javascript, "./storage.ffi.mjs", "onHashChange")
pub fn on_hash_change(callback: fn(String) -> Nil) -> Nil
