//// FFI bindings around localStorage + URL-hash routing.

@external(javascript, "./storage.ffi.mjs", "readJoinedRooms")
pub fn read_joined_rooms() -> List(String)

@external(javascript, "./storage.ffi.mjs", "writeJoinedRooms")
pub fn write_joined_rooms(rooms: List(String)) -> Nil

@external(javascript, "./storage.ffi.mjs", "readHash")
pub fn read_hash() -> String

@external(javascript, "./storage.ffi.mjs", "setHash")
pub fn set_hash(name: String) -> Nil

/// Register a callback that fires every time the URL hash changes.
@external(javascript, "./storage.ffi.mjs", "onHashChange")
pub fn on_hash_change(callback: fn(String) -> Nil) -> Nil

/// "" when the user has never picked a theme (in which case we follow
/// `prefers_dark` instead). "light" or "dark" once they've toggled.
@external(javascript, "./storage.ffi.mjs", "readSavedTheme")
pub fn read_saved_theme() -> String

@external(javascript, "./storage.ffi.mjs", "writeSavedTheme")
pub fn write_saved_theme(value: String) -> Nil

/// True when the OS / browser is currently advertising a dark colour
/// scheme via prefers-color-scheme. Used as the fallback when the
/// user hasn't toggled the theme yet.
@external(javascript, "./storage.ffi.mjs", "prefersDark")
pub fn prefers_dark() -> Bool

/// True when the current viewport width is <= 767px (phone tier).
@external(javascript, "./storage.ffi.mjs", "isPhoneViewport")
pub fn is_phone_viewport() -> Bool

/// Register a callback that fires whenever the viewport crosses the
/// 768px boundary (in either direction). Fires once per crossing, not
/// on every resize.
@external(javascript, "./storage.ffi.mjs", "onViewportChange")
pub fn on_viewport_change(callback: fn(Bool) -> Nil) -> Nil

/// Wipe localStorage / sessionStorage / hash and reload the page so
/// the user gets a clean slate. Triggered by the "reset local state"
/// settings action — this also drops the persisted identity keypair,
/// so the user comes back as a fresh peer.
@external(javascript, "./storage.ffi.mjs", "resetLocalStateAndReload")
pub fn reset_local_state_and_reload() -> Nil

/// Replace the default `<meta name="viewport">` with a mobile-friendly
/// one that enables safe-area insets and keyboard-aware resizing.
@external(javascript, "./storage.ffi.mjs", "installMobileViewportMeta")
pub fn install_mobile_viewport_meta() -> Nil
